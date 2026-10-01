use std::collections::HashMap;

use serde::Serialize;
use serde_json::Value;
use thiserror::Error;

use crate::model::WaveFrame;

/// 前端展示和创建输入源时所需的稳定元数据。
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceDescriptor {
    pub kind: &'static str,
    pub display_name: &'static str,
    pub description: &'static str,
}

#[derive(Debug, Error)]
pub enum SourceError {
    #[error("未知输入源类型：{0}")]
    UnknownKind(String),
    #[error("输入源类型重复注册：{0}")]
    DuplicateKind(String),
    #[error("输入源 {kind} 的配置无效：{message}")]
    InvalidConfig { kind: &'static str, message: String },
    #[error("输入源运行失败：{0}")]
    Runtime(String),
}

/// Hub 每 100ms 拉取一帧；实现不能在该方法中长期阻塞。
pub trait WaveSource: Send {
    fn next_frame(&mut self) -> Result<WaveFrame, SourceError>;
}

/// 编译期输入源扩展点。
pub trait SourceFactory: Send + Sync {
    fn descriptor(&self) -> SourceDescriptor;

    fn default_config(&self) -> Value;

    fn validate(&self, config: &Value) -> Result<(), SourceError>;

    fn build(&self, config: &Value) -> Result<Box<dyn WaveSource>, SourceError>;
}

/// 保持注册顺序的 factory 注册表，便于 UI 稳定展示。
pub struct SourceRegistry {
    factories: Vec<Box<dyn SourceFactory>>,
    indices: HashMap<&'static str, usize>,
    descriptors: Vec<SourceDescriptor>,
}

impl SourceRegistry {
    pub fn new(
        factories: impl IntoIterator<Item = Box<dyn SourceFactory>>,
    ) -> Result<Self, SourceError> {
        let factories: Vec<_> = factories.into_iter().collect();
        let mut indices = HashMap::with_capacity(factories.len());
        let mut descriptors = Vec::with_capacity(factories.len());

        for (index, factory) in factories.iter().enumerate() {
            let descriptor = factory.descriptor();
            if indices.insert(descriptor.kind, index).is_some() {
                return Err(SourceError::DuplicateKind(descriptor.kind.to_owned()));
            }
            descriptors.push(descriptor);
        }

        Ok(Self {
            factories,
            indices,
            descriptors,
        })
    }

    pub fn list_descriptors(&self) -> &[SourceDescriptor] {
        &self.descriptors
    }

    pub fn default_config(&self, kind: &str) -> Result<Value, SourceError> {
        Ok(self.factory(kind)?.default_config())
    }

    pub fn validate(&self, kind: &str, config: &Value) -> Result<(), SourceError> {
        self.factory(kind)?.validate(config)
    }

    pub fn build(&self, kind: &str, config: &Value) -> Result<Box<dyn WaveSource>, SourceError> {
        self.factory(kind)?.build(config)
    }

    fn factory(&self, kind: &str) -> Result<&dyn SourceFactory, SourceError> {
        self.indices
            .get(kind)
            .map(|index| self.factories[*index].as_ref())
            .ok_or_else(|| SourceError::UnknownKind(kind.to_owned()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct StubFactory;

    impl SourceFactory for StubFactory {
        fn descriptor(&self) -> SourceDescriptor {
            SourceDescriptor {
                kind: "test.stub",
                display_name: "Stub",
                description: "",
            }
        }

        fn default_config(&self) -> Value {
            Value::Null
        }

        fn validate(&self, _config: &Value) -> Result<(), SourceError> {
            Ok(())
        }

        fn build(&self, _config: &Value) -> Result<Box<dyn WaveSource>, SourceError> {
            unreachable!()
        }
    }

    #[test]
    fn duplicate_kinds_are_rejected_at_startup() {
        let result = SourceRegistry::new([
            Box::new(StubFactory) as Box<dyn SourceFactory>,
            Box::new(StubFactory) as Box<dyn SourceFactory>,
        ]);

        assert!(matches!(result, Err(SourceError::DuplicateKind(kind)) if kind == "test.stub"));
    }

    #[test]
    fn unknown_kind_is_explicit() {
        let registry = SourceRegistry::new([]).unwrap();
        assert!(matches!(
            registry.build("missing", &Value::Null),
            Err(SourceError::UnknownKind(kind)) if kind == "missing"
        ));
    }
}
