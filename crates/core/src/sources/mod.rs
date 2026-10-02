pub mod audio;
pub mod mapping;
mod preset;
mod registry;
pub mod touch;

pub use preset::{DEFAULT_WAVEFORM_ID, FixedWaveformFactory, WaveformConfig};
pub use registry::{SourceDescriptor, SourceError, SourceFactory, SourceRegistry, WaveSource};

/// 固定波形是唯一核心基础输入源；动态输入源由插件管理器提供。
///
/// 固定波形是唯一的核心输入源；扩展输入源经原生插件运行，核心不引入
/// Rust ABI 不稳定的动态库插件。
pub fn builtin_registry() -> SourceRegistry {
    SourceRegistry::new([Box::new(FixedWaveformFactory) as Box<dyn SourceFactory>])
        .expect("内置输入源 kind 必须唯一")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_registry_is_complete_and_stably_ordered() {
        let registry = builtin_registry();
        let kinds: Vec<_> = registry
            .list_descriptors()
            .iter()
            .map(|descriptor| descriptor.kind)
            .collect();

        assert_eq!(kinds, ["builtin.fixed_waveform"]);
    }
}
