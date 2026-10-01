use std::sync::Arc;

use futures_util::future::BoxFuture;
use futures_util::stream::BoxStream;

use super::super::{BluetoothDevice, TransportError};

#[derive(Debug, Clone)]
pub struct GattInfo {
    pub name: String,
    pub battery: Option<u16>,
    pub firmware: Option<u8>,
    pub battery_notifications: bool,
    pub load_notifications: bool,
}

#[derive(Debug, Clone)]
pub enum GattNotification {
    Message(Vec<u8>),
    Battery(u8),
    Load([u8; 2]),
}

/// Future-based boundary keeps native GATT out of protocol and actor tests.
pub trait GattBackend: Send + Sync {
    fn write<'a>(&'a self, value: &'a [u8]) -> BoxFuture<'a, Result<(), TransportError>>;
    fn disconnect(&self) -> BoxFuture<'_, Result<(), TransportError>>;
}

pub struct GattConnection {
    pub backend: Arc<dyn GattBackend>,
    pub notifications: BoxStream<'static, GattNotification>,
    pub info: GattInfo,
}

pub trait GattFactory: Send + Sync {
    fn open<'a>(
        &'a self,
        device_id: &'a str,
    ) -> BoxFuture<'a, Result<GattConnection, TransportError>>;
}

pub struct NativeFactory;

#[cfg(not(target_os = "windows"))]
impl GattFactory for NativeFactory {
    fn open<'a>(
        &'a self,
        _device_id: &'a str,
    ) -> BoxFuture<'a, Result<GattConnection, TransportError>> {
        Box::pin(async { Err(unsupported()) })
    }
}

#[cfg(not(target_os = "windows"))]
fn unsupported() -> TransportError {
    TransportError::new("bluetooth_unsupported", "蓝牙直接连接首版仅支持 Windows")
}

pub async fn scan(duration_ms: u64) -> Result<Vec<BluetoothDevice>, TransportError> {
    if !(100..=10_000).contains(&duration_ms) {
        return Err(TransportError::new(
            "invalid_scan_duration",
            "蓝牙扫描时间须为 100..10000ms",
        ));
    }
    #[cfg(target_os = "windows")]
    {
        // Bound adapter discovery and native operations as well as the observation window.
        let budget =
            std::time::Duration::from_millis(duration_ms) + std::time::Duration::from_secs(3);
        tokio::time::timeout(budget, native::scan(duration_ms))
            .await
            .map_err(|_| {
                TransportError::new("bluetooth_scan_timeout", "蓝牙扫描超时，正在停止原生扫描")
            })?
    }
    #[cfg(not(target_os = "windows"))]
    {
        Err(unsupported())
    }
}

#[cfg(target_os = "windows")]
impl GattFactory for NativeFactory {
    fn open<'a>(
        &'a self,
        device_id: &'a str,
    ) -> BoxFuture<'a, Result<GattConnection, TransportError>> {
        Box::pin(native::open(device_id))
    }
}

#[cfg(target_os = "windows")]
mod native {
    use std::collections::BTreeMap;
    use std::sync::{OnceLock, Weak};
    use std::time::Duration;

    use btleplug::api::{
        Central, CharPropFlags, Characteristic, Manager as _, Peripheral as _, ScanFilter,
        WriteType,
    };
    use btleplug::platform::{Adapter, Manager, Peripheral};
    use futures_util::StreamExt;
    use tokio::sync::{Mutex, OwnedMutexGuard};
    use tokio::time::{Instant, sleep, timeout};
    use uuid::Uuid;

    use super::*;

    const MAX_DISCOVERIES: usize = 64;
    const DEVICE_NAME: &str = "47L121000";
    const SERVICE_CONTROL: Uuid = Uuid::from_u128(0x0000180c00001000800000805f9b34fb);
    const SERVICE_INFO: Uuid = Uuid::from_u128(0x0000180a00001000800000805f9b34fb);
    const WRITE: Uuid = Uuid::from_u128(0x0000150a00001000800000805f9b34fb);
    const MESSAGE: Uuid = Uuid::from_u128(0x0000150b00001000800000805f9b34fb);
    const BATTERY: Uuid = Uuid::from_u128(0x0000150000001000800000805f9b34fb);
    const VERSION: Uuid = Uuid::from_u128(0x0000150100001000800000805f9b34fb);
    const LOAD: Uuid = Uuid::from_u128(0x00002a5900001000800000805f9b34fb);

    fn cache() -> &'static Mutex<BTreeMap<String, Peripheral>> {
        static CACHE: OnceLock<Mutex<BTreeMap<String, Peripheral>>> = OnceLock::new();
        CACHE.get_or_init(Mutex::default)
    }

    fn native_error(error: btleplug::Error) -> TransportError {
        let code = match &error {
            btleplug::Error::PermissionDenied => "bluetooth_permission_denied",
            btleplug::Error::NotSupported(_) | btleplug::Error::NoAdapterAvailable => {
                "bluetooth_unavailable"
            }
            _ => "bluetooth_transport",
        };
        TransportError::new(code, format!("蓝牙操作失败：{error}"))
    }

    fn scan_lock() -> &'static Arc<Mutex<()>> {
        static LOCK: OnceLock<Arc<Mutex<()>>> = OnceLock::new();
        LOCK.get_or_init(|| Arc::new(Mutex::new(())))
    }

    fn scan_lease() -> Result<OwnedMutexGuard<()>, TransportError> {
        scan_lock().clone().try_lock_owned().map_err(|_| {
            TransportError::new("bluetooth_scan_busy", "蓝牙扫描正在进行或清理，请稍后重试")
        })
    }

    struct ScanCleanup {
        adapters: Vec<Adapter>,
        lease: Option<OwnedMutexGuard<()>>,
    }

    impl ScanCleanup {
        async fn stop(&mut self) -> Result<(), TransportError> {
            let stopped = timeout(
                Duration::from_secs(2),
                futures_util::future::join_all(
                    self.adapters.iter().map(|adapter| adapter.stop_scan()),
                ),
            )
            .await
            .map_err(|_| TransportError::new("bluetooth_scan_stop_timeout", "停止蓝牙扫描超时"))?;
            for result in stopped {
                result.map_err(native_error)?;
            }
            self.adapters.clear();
            Ok(())
        }
    }

    impl Drop for ScanCleanup {
        fn drop(&mut self) {
            if self.adapters.is_empty() {
                return;
            }
            if let Ok(runtime) = tokio::runtime::Handle::try_current() {
                let adapters = std::mem::take(&mut self.adapters);
                // Keep serialization held until cancelled native scan operations settle.
                let lease = self.lease.take();
                runtime.spawn(async move {
                    let _lease = lease;
                    let _ = timeout(
                        Duration::from_secs(2),
                        futures_util::future::join_all(
                            adapters.iter().map(|adapter| adapter.stop_scan()),
                        ),
                    )
                    .await;
                });
            }
        }
    }

    fn device_lease(device_id: &str) -> Result<Arc<DeviceLease>, TransportError> {
        type Locks = BTreeMap<String, Weak<Mutex<()>>>;
        static LOCKS: OnceLock<std::sync::Mutex<Locks>> = OnceLock::new();
        let mut locks = LOCKS
            .get_or_init(std::sync::Mutex::default)
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        locks.retain(|_, lock| lock.strong_count() != 0);
        let lock = if let Some(lock) = locks.get(device_id).and_then(Weak::upgrade) {
            lock
        } else {
            if locks.len() >= MAX_DISCOVERIES {
                return Err(TransportError::busy());
            }
            let lock = Arc::new(Mutex::new(()));
            locks.insert(device_id.to_owned(), Arc::downgrade(&lock));
            lock
        };
        let guard = lock.clone().try_lock_owned().map_err(|_| {
            TransportError::new(
                "bluetooth_device_busy",
                "该蓝牙设备正在使用或清理，请稍后重试",
            )
        })?;
        Ok(Arc::new(DeviceLease {
            _lock: lock,
            _guard: guard,
        }))
    }

    struct DeviceLease {
        _lock: Arc<Mutex<()>>,
        _guard: OwnedMutexGuard<()>,
    }

    /// A native connect may settle after cancellation of its Rust future.
    struct ConnectCleanup(Option<(Peripheral, Arc<DeviceLease>)>);
    impl Drop for ConnectCleanup {
        fn drop(&mut self) {
            if let Some((peripheral, lease)) = self.0.take()
                && let Ok(runtime) = tokio::runtime::Handle::try_current()
            {
                runtime.spawn(async move {
                    let _lease = lease;
                    let deadline = Instant::now() + Duration::from_secs(3);
                    loop {
                        if peripheral.is_connected().await.unwrap_or(false) {
                            let _ = timeout(Duration::from_secs(1), peripheral.disconnect()).await;
                        }
                        if Instant::now() >= deadline {
                            break;
                        }
                        sleep(Duration::from_millis(100)).await;
                    }
                });
            }
        }
    }

    pub async fn scan(duration_ms: u64) -> Result<Vec<BluetoothDevice>, TransportError> {
        let lease = scan_lease()?;
        let manager = Manager::new().await.map_err(native_error)?;
        let adapters = manager.adapters().await.map_err(native_error)?;
        if adapters.is_empty() {
            return Err(TransportError::new(
                "bluetooth_unavailable",
                "未找到可用蓝牙适配器",
            ));
        }
        let mut cleanup = ScanCleanup {
            adapters: adapters.clone(),
            lease: Some(lease),
        };
        // The cleanup also owns adapters with an in-flight start_scan during cancellation.
        for adapter in &adapters {
            adapter
                .start_scan(ScanFilter::default())
                .await
                .map_err(native_error)?;
        }
        let deadline = Instant::now() + Duration::from_millis(duration_ms);
        let mut results = BTreeMap::new();
        let mut peripherals = BTreeMap::new();
        let result = async {
            loop {
                for adapter in &adapters {
                    for peripheral in adapter.peripherals().await.map_err(native_error)? {
                        let Some(properties) =
                            peripheral.properties().await.map_err(native_error)?
                        else {
                            continue;
                        };
                        if properties.local_name.as_deref() != Some(DEVICE_NAME) {
                            continue;
                        }
                        let device_id = peripheral.id().to_string();
                        if !results.contains_key(&device_id) && results.len() >= MAX_DISCOVERIES {
                            continue;
                        }
                        results.insert(
                            device_id.clone(),
                            BluetoothDevice {
                                device_id: device_id.clone(),
                                name: DEVICE_NAME.to_owned(),
                                rssi: properties.rssi,
                            },
                        );
                        peripherals.insert(device_id, peripheral);
                    }
                }
                if Instant::now() >= deadline {
                    break;
                }
                sleep(Duration::from_millis(100)).await;
            }
            Ok::<_, TransportError>(())
        }
        .await;
        cleanup.stop().await?;
        result?;
        *cache().lock().await = peripherals;
        Ok(results.into_values().collect())
    }

    pub async fn open(device_id: &str) -> Result<GattConnection, TransportError> {
        let lease = device_lease(device_id)?;
        let peripheral = cache()
            .lock()
            .await
            .get(device_id)
            .cloned()
            .ok_or_else(|| {
                TransportError::new("bluetooth_not_found", "未找到该蓝牙设备，请先执行扫描")
            })?;
        let properties = peripheral.properties().await.map_err(native_error)?;
        if properties.as_ref().and_then(|p| p.local_name.as_deref()) != Some(DEVICE_NAME) {
            return Err(TransportError::new(
                "bluetooth_unsupported_device",
                "仅支持郊狼 3.0 脉冲主机",
            ));
        }
        let mut cleanup = ConnectCleanup(Some((peripheral.clone(), Arc::clone(&lease))));
        if !peripheral.is_connected().await.map_err(native_error)? {
            timeout(Duration::from_secs(3), peripheral.connect())
                .await
                .map_err(|_| TransportError::new("bluetooth_connect_timeout", "连接蓝牙设备超时"))?
                .map_err(native_error)?;
        }
        let result = prepare(peripheral.clone(), lease).await;
        if result.is_ok() {
            cleanup.0 = None;
        }
        result
    }

    async fn prepare(
        peripheral: Peripheral,
        lease: Arc<DeviceLease>,
    ) -> Result<GattConnection, TransportError> {
        peripheral.discover_services().await.map_err(native_error)?;
        let characteristics = peripheral.characteristics();
        let write = characteristics
            .iter()
            .find(|c| {
                c.uuid == WRITE
                    && c.service_uuid == SERVICE_CONTROL
                    && c.properties
                        .intersects(CharPropFlags::WRITE | CharPropFlags::WRITE_WITHOUT_RESPONSE)
            })
            .cloned()
            .ok_or_else(|| {
                TransportError::new("bluetooth_invalid_gatt", "设备缺少郊狼 3.0 写特征")
            })?;
        let message = characteristics
            .iter()
            .find(|c| {
                c.uuid == MESSAGE
                    && c.service_uuid == SERVICE_CONTROL
                    && c.properties.contains(CharPropFlags::NOTIFY)
            })
            .ok_or_else(|| {
                TransportError::new("bluetooth_invalid_gatt", "设备缺少郊狼 3.0 通知特征")
            })?;
        let notifications = peripheral.notifications().await.map_err(native_error)?;
        peripheral.subscribe(message).await.map_err(native_error)?;
        let battery = characteristics
            .iter()
            .find(|c| c.uuid == BATTERY && c.service_uuid == SERVICE_INFO);
        let version = characteristics
            .iter()
            .find(|c| c.uuid == VERSION && c.service_uuid == SERVICE_INFO);
        let load = characteristics
            .iter()
            .find(|c| c.uuid == LOAD && c.service_uuid == SERVICE_INFO);
        let power = if let Some(battery) = battery {
            peripheral
                .read(battery)
                .await
                .ok()
                .and_then(|v| v.first().copied())
                .filter(|p| *p <= 100)
                .map(u16::from)
        } else {
            None
        };
        let firmware = if let Some(version) = version {
            peripheral
                .read(version)
                .await
                .ok()
                .and_then(|v| v.first().copied())
        } else {
            None
        };
        let battery_notifications = if let Some(battery) = battery {
            battery.properties.contains(CharPropFlags::NOTIFY)
                && peripheral.subscribe(battery).await.is_ok()
        } else {
            false
        };
        let load_notifications = if let Some(load) = load {
            load.properties.contains(CharPropFlags::NOTIFY)
                && peripheral.subscribe(load).await.is_ok()
        } else {
            false
        };
        let stream = notifications
            .filter_map(|notification| async move {
                if notification.uuid == MESSAGE {
                    if notification.value.len() <= 20 {
                        Some(GattNotification::Message(notification.value))
                    } else {
                        None
                    }
                } else if notification.uuid == BATTERY {
                    notification
                        .value
                        .first()
                        .copied()
                        .filter(|p| *p <= 100)
                        .map(GattNotification::Battery)
                } else if notification.uuid == LOAD && notification.value.len() == 2 {
                    Some(GattNotification::Load([
                        notification.value[0],
                        notification.value[1],
                    ]))
                } else {
                    None
                }
            })
            .boxed();
        let write_type = if write
            .properties
            .contains(CharPropFlags::WRITE_WITHOUT_RESPONSE)
        {
            WriteType::WithoutResponse
        } else {
            WriteType::WithResponse
        };
        Ok(GattConnection {
            backend: Arc::new(NativeBackend {
                peripheral,
                write,
                write_type,
                _lease: lease,
            }),
            notifications: stream,
            info: GattInfo {
                name: DEVICE_NAME.to_owned(),
                battery: power,
                firmware,
                battery_notifications,
                load_notifications,
            },
        })
    }

    struct NativeBackend {
        peripheral: Peripheral,
        write: Characteristic,
        write_type: WriteType,
        _lease: Arc<DeviceLease>,
    }
    impl GattBackend for NativeBackend {
        fn write<'a>(&'a self, value: &'a [u8]) -> BoxFuture<'a, Result<(), TransportError>> {
            Box::pin(async move {
                self.peripheral
                    .write(&self.write, value, self.write_type)
                    .await
                    .map_err(native_error)
            })
        }
        fn disconnect(&self) -> BoxFuture<'_, Result<(), TransportError>> {
            Box::pin(async move { self.peripheral.disconnect().await.map_err(native_error) })
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn native_errors_distinguish_permission_and_adapter_availability() {
            assert_eq!(
                native_error(btleplug::Error::PermissionDenied).code,
                "bluetooth_permission_denied"
            );
            assert_eq!(
                native_error(btleplug::Error::NotSupported("disabled".to_owned())).code,
                "bluetooth_unavailable"
            );
            assert_eq!(
                native_error(btleplug::Error::NoAdapterAvailable).code,
                "bluetooth_unavailable"
            );
            assert_eq!(
                native_error(btleplug::Error::NotConnected).code,
                "bluetooth_transport"
            );
        }

        #[test]
        fn scan_lease_rejects_concurrent_scan_without_contacting_os() {
            let lease = scan_lease().unwrap();
            assert_eq!(scan_lease().unwrap_err().code, "bluetooth_scan_busy");
            drop(lease);
            assert!(scan_lease().is_ok());
        }

        #[test]
        fn old_connection_cleanup_keeps_device_exclusive_until_its_last_owner_exits() {
            let lease = device_lease("native-lease-test").unwrap();
            let cleanup_owner = Arc::clone(&lease);
            drop(lease);
            assert!(
                matches!(device_lease("native-lease-test"), Err(error) if error.code == "bluetooth_device_busy")
            );
            drop(cleanup_owner);
            assert!(device_lease("native-lease-test").is_ok());
        }
    }
}
