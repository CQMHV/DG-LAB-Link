mod preset;
mod registry;

pub use preset::{DEFAULT_WAVEFORM_ID, FixedWaveformFactory, WaveformConfig};
pub use registry::{SourceDescriptor, SourceError, SourceFactory, SourceRegistry, WaveSource};

/// 返回项目随二进制编译的输入源注册表。
///
/// 增加内置输入源时在此处显式注册，以便启动时即可发现重复 kind，且不引入
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
