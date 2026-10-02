//! Local ZIP package validation, extraction and authoring helpers.
use std::collections::HashSet;
use std::fs::{self, File};
use std::io::{Read, Seek, Write};
use std::path::{Path, PathBuf};

use dg_lab_link_plugin_sdk::{PROTOCOL_VERSION, PluginError, PluginManifest};
use sha2::{Digest, Sha256};
use zip::write::SimpleFileOptions;

pub const MAX_PACKAGE_BYTES: u64 = 64 * 1024 * 1024;
pub const MAX_EXPANDED_BYTES: u64 = 256 * 1024 * 1024;
pub const MAX_PACKAGE_ENTRIES: usize = 1024;
const MAX_MANIFEST_BYTES: u64 = 64 * 1024;

pub struct PreparedPackage {
    pub manifest: PluginManifest,
    pub digest: String,
    pub directory: PathBuf,
}

pub fn validate_manifest(manifest: &PluginManifest) -> Result<(), PluginError> {
    if manifest.protocol_version != PROTOCOL_VERSION {
        return Err(PluginError::new(
            "protocol_incompatible",
            "插件协议版本不兼容",
        ));
    }
    if manifest.id.len() > 128
        || !manifest.id.contains('.')
        || manifest.id.split('.').any(|part| {
            part.is_empty()
                || !part
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        })
    {
        return Err(PluginError::new(
            "invalid_package",
            "插件 ID 必须为小写反向域名格式",
        ));
    }
    if manifest.version.is_empty()
        || manifest.version.len() > 64
        || manifest.version.contains(['/', '\\', ':'])
        || [
            manifest.name.as_str(),
            manifest.publisher.as_str(),
            manifest.license.as_str(),
        ]
        .iter()
        .any(|text| text.is_empty() || text.len() > 256)
    {
        return Err(PluginError::new(
            "invalid_package",
            "插件版本或基本元数据无效",
        ));
    }
    validate_relative_path(&manifest.executable)?;
    if !manifest.executable.to_ascii_lowercase().ends_with(".exe") {
        return Err(PluginError::new(
            "invalid_package",
            "Windows 插件入口必须为 .exe 文件",
        ));
    }
    Ok(())
}

/// Windows path rules are checked on every platform, including DOS aliases.
pub fn validate_relative_path(name: &str) -> Result<PathBuf, PluginError> {
    if name.is_empty()
        || name.len() > 512
        || name.contains(['\\', ':', '\0'])
        || name.starts_with('/')
    {
        return Err(PluginError::new("invalid_package", "插件包包含非法路径"));
    }
    for part in name.trim_end_matches('/').split('/') {
        if part.is_empty()
            || part == "."
            || part == ".."
            || part.ends_with(['.', ' '])
            || part.chars().any(char::is_control)
        {
            return Err(PluginError::new(
                "invalid_package",
                "插件包包含路径穿越或 Windows 路径别名",
            ));
        }
        let base = part.split('.').next().unwrap_or(part).to_ascii_uppercase();
        if matches!(
            base.as_str(),
            "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
        ) || base
            .strip_prefix("COM")
            .or_else(|| base.strip_prefix("LPT"))
            .is_some_and(|suffix| suffix.len() == 1 && matches!(suffix.as_bytes()[0], b'1'..=b'9'))
        {
            return Err(PluginError::new(
                "invalid_package",
                "插件包包含 Windows 保留设备路径",
            ));
        }
    }
    Ok(PathBuf::from(name.trim_end_matches('/')))
}

pub fn prepare_package(
    package: &Path,
    staging_parent: &Path,
) -> Result<PreparedPackage, PluginError> {
    if package
        .extension()
        .and_then(|value| value.to_str())
        .is_none_or(|extension| !extension.eq_ignore_ascii_case("dglabplugin"))
    {
        return Err(PluginError::new(
            "invalid_package",
            "请选择 .dglabplugin 本地插件包",
        ));
    }
    let metadata = fs::metadata(package).map_err(io_error)?;
    if !metadata.is_file() || metadata.len() > MAX_PACKAGE_BYTES {
        return Err(PluginError::new("package_too_large", "插件包超过 64 MiB"));
    }
    let mut file = File::open(package).map_err(io_error)?;
    let mut hash = Sha256::new();
    let mut buffer = [0_u8; 65536];
    loop {
        let length = file.read(&mut buffer).map_err(io_error)?;
        if length == 0 {
            break;
        }
        hash.update(&buffer[..length]);
    }
    let digest = format!("{:x}", hash.finalize());
    file.rewind().map_err(io_error)?;
    let mut archive = zip::ZipArchive::new(file).map_err(zip_error)?;
    if archive.len() > MAX_PACKAGE_ENTRIES {
        return Err(PluginError::new(
            "package_too_large",
            "插件包条目超过 1024 项",
        ));
    }
    let mut names = HashSet::new();
    let mut expanded = 0_u64;
    for index in 0..archive.len() {
        let entry = archive.by_index(index).map_err(zip_error)?;
        let name = std::str::from_utf8(entry.name_raw())
            .map_err(|_| PluginError::new("invalid_package", "插件路径必须使用 UTF-8"))?;
        validate_relative_path(name)?;
        if !names.insert(name.trim_end_matches('/').to_lowercase()) {
            return Err(PluginError::new(
                "invalid_package",
                "插件包包含重复或大小写冲突路径",
            ));
        }
        if entry
            .unix_mode()
            .is_some_and(|mode| !matches!(mode & 0o170000, 0 | 0o100000 | 0o040000))
        {
            return Err(PluginError::new(
                "invalid_package",
                "插件包禁止符号链接及特殊文件",
            ));
        }
        expanded = expanded
            .checked_add(entry.size())
            .ok_or_else(|| PluginError::new("package_too_large", "插件展开大小溢出"))?;
        if expanded > MAX_EXPANDED_BYTES {
            return Err(PluginError::new(
                "package_too_large",
                "插件展开后超过 256 MiB",
            ));
        }
    }
    let manifest: PluginManifest = {
        let entry = archive
            .by_name("plugin.json")
            .map_err(|_| PluginError::new("invalid_package", "插件包缺少 plugin.json"))?;
        if entry.size() > MAX_MANIFEST_BYTES {
            return Err(PluginError::new("invalid_package", "插件清单过大"));
        }
        serde_json::from_reader(entry.take(MAX_MANIFEST_BYTES + 1))
            .map_err(|error| PluginError::new("invalid_package", error.to_string()))?
    };
    validate_manifest(&manifest)?;
    {
        let executable = archive
            .by_name(&manifest.executable)
            .map_err(|_| PluginError::new("invalid_package", "插件入口文件不存在"))?;
        if !executable.is_file() || executable.size() == 0 {
            return Err(PluginError::new("invalid_package", "插件入口不是有效文件"));
        }
    }
    fs::create_dir_all(staging_parent).map_err(io_error)?;
    let directory = staging_parent.join(uuid::Uuid::new_v4().to_string());
    fs::create_dir(&directory).map_err(io_error)?;
    let result = (|| {
        for index in 0..archive.len() {
            let mut entry = archive.by_index(index).map_err(zip_error)?;
            let destination = directory.join(validate_relative_path(entry.name())?);
            if entry.is_dir() {
                fs::create_dir_all(&destination).map_err(io_error)?;
                continue;
            }
            if let Some(parent) = destination.parent() {
                fs::create_dir_all(parent).map_err(io_error)?;
            }
            let mut output = File::options()
                .write(true)
                .create_new(true)
                .open(&destination)
                .map_err(io_error)?;
            let copied = std::io::copy(
                &mut entry.by_ref().take(MAX_EXPANDED_BYTES + 1),
                &mut output,
            )
            .map_err(io_error)?;
            if copied != entry.size() {
                return Err(PluginError::new("invalid_package", "插件文件长度不匹配"));
            }
            output.sync_all().map_err(io_error)?;
        }
        validate_windows_executable(&directory.join(&manifest.executable))?;
        Ok(PreparedPackage {
            manifest,
            digest,
            directory: directory.clone(),
        })
    })();
    if result.is_err() {
        let _ = fs::remove_dir_all(&directory);
    }
    result
}

/// Package an SDK project output directory without scripts or shell execution.
pub fn pack_directory(directory: &Path, destination: &Path) -> Result<PluginManifest, PluginError> {
    let manifest: PluginManifest =
        serde_json::from_reader(File::open(directory.join("plugin.json")).map_err(io_error)?)
            .map_err(|error| PluginError::new("invalid_package", error.to_string()))?;
    validate_manifest(&manifest)?;
    if !directory.join(&manifest.executable).is_file() {
        return Err(PluginError::new("invalid_package", "插件入口文件不存在"));
    }
    validate_windows_executable(&directory.join(&manifest.executable))?;
    let mut entries = Vec::new();
    collect_files(directory, directory, &mut entries)?;
    if entries.len() > MAX_PACKAGE_ENTRIES {
        return Err(PluginError::new("package_too_large", "插件包条目过多"));
    }
    entries.sort();
    let file = File::create(destination).map_err(io_error)?;
    let mut writer = zip::ZipWriter::new(file);
    let options = SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated)
        .unix_permissions(0o644);
    let result = (|| {
        let mut total = 0_u64;
        for relative in entries {
            let name = relative.to_string_lossy().replace('\\', "/");
            validate_relative_path(&name)?;
            let mut input = File::open(directory.join(&relative)).map_err(io_error)?;
            total = total.saturating_add(input.metadata().map_err(io_error)?.len());
            if total > MAX_EXPANDED_BYTES {
                return Err(PluginError::new(
                    "package_too_large",
                    "插件展开后超过 256 MiB",
                ));
            }
            writer.start_file(name, options).map_err(zip_error)?;
            std::io::copy(&mut input, &mut writer).map_err(io_error)?;
        }
        let mut file = writer.finish().map_err(zip_error)?;
        file.flush().map_err(io_error)?;
        file.sync_all().map_err(io_error)?;
        if file.metadata().map_err(io_error)?.len() > MAX_PACKAGE_BYTES {
            return Err(PluginError::new(
                "package_too_large",
                "压缩插件包超过 64 MiB",
            ));
        }
        Ok(manifest)
    })();
    if result.is_err() {
        let _ = fs::remove_file(destination);
    }
    result
}

fn validate_windows_executable(path: &Path) -> Result<(), PluginError> {
    let mut file = File::open(path).map_err(io_error)?;
    let mut header = [0_u8; 64];
    file.read_exact(&mut header)
        .map_err(|_| PluginError::new("invalid_package", "插件入口缺少 Windows PE 头"))?;
    if &header[..2] != b"MZ" {
        return Err(PluginError::new(
            "invalid_package",
            "插件入口不是 Windows PE 可执行文件",
        ));
    }
    let offset = u32::from_le_bytes(header[60..64].try_into().expect("four bytes")) as u64;
    if offset < 64 || offset.saturating_add(4) > file.metadata().map_err(io_error)?.len() {
        return Err(PluginError::new("invalid_package", "Windows PE 头偏移无效"));
    }
    file.seek(std::io::SeekFrom::Start(offset))
        .map_err(io_error)?;
    let mut signature = [0_u8; 4];
    file.read_exact(&mut signature).map_err(io_error)?;
    if &signature != b"PE\0\0" {
        return Err(PluginError::new("invalid_package", "Windows PE 签名无效"));
    }
    Ok(())
}

#[cfg(test)]
pub(crate) fn executable_fixture() -> Vec<u8> {
    let mut bytes = vec![0_u8; 68];
    bytes[..2].copy_from_slice(b"MZ");
    bytes[60..64].copy_from_slice(&64_u32.to_le_bytes());
    bytes[64..68].copy_from_slice(b"PE\0\0");
    bytes
}

fn collect_files(
    root: &Path,
    directory: &Path,
    result: &mut Vec<PathBuf>,
) -> Result<(), PluginError> {
    for entry in fs::read_dir(directory).map_err(io_error)? {
        let entry = entry.map_err(io_error)?;
        let metadata = entry.file_type().map_err(io_error)?;
        if metadata.is_symlink() {
            return Err(PluginError::new("invalid_package", "打包目录禁止符号链接"));
        }
        if metadata.is_dir() {
            collect_files(root, &entry.path(), result)?;
        } else if metadata.is_file() {
            result.push(
                entry
                    .path()
                    .strip_prefix(root)
                    .map_err(|error| PluginError::new("invalid_package", error.to_string()))?
                    .to_path_buf(),
            );
        } else {
            return Err(PluginError::new("invalid_package", "打包目录包含特殊文件"));
        }
        if result.len() > MAX_PACKAGE_ENTRIES {
            return Err(PluginError::new("package_too_large", "插件包条目过多"));
        }
    }
    Ok(())
}

pub(crate) fn io_error(error: std::io::Error) -> PluginError {
    PluginError::new("plugin_io", error.to_string())
}
fn zip_error(error: zip::result::ZipError) -> PluginError {
    PluginError::new("invalid_package", error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest() -> PluginManifest {
        PluginManifest {
            id: "example.source".into(),
            version: "1.0.0".into(),
            protocol_version: 1,
            name: "示例".into(),
            publisher: "Example".into(),
            license: "MIT".into(),
            executable: "bin/source.exe".into(),
        }
    }

    #[test]
    fn validates_windows_aliases_and_traversal() {
        for name in [
            "../out",
            "/absolute",
            "C:/absolute",
            "a\\b",
            "a/./b",
            "a//b",
            "CON.txt",
            "bin/LPT1",
            "bin/name.",
            "bin/name ",
        ] {
            assert!(validate_relative_path(name).is_err(), "{name}");
        }
        assert_eq!(
            validate_relative_path("bin/source.exe").unwrap(),
            PathBuf::from("bin/source.exe")
        );
    }

    #[test]
    fn roundtrip_package_and_reject_case_collision() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        fs::create_dir_all(source.join("bin")).unwrap();
        fs::write(
            source.join("plugin.json"),
            serde_json::to_vec(&manifest()).unwrap(),
        )
        .unwrap();
        fs::write(source.join("bin/source.exe"), executable_fixture()).unwrap();
        let package = temp.path().join("source.dglabplugin");
        pack_directory(&source, &package).unwrap();
        let prepared = prepare_package(&package, &temp.path().join("stage")).unwrap();
        assert_eq!(prepared.manifest.id, "example.source");
        assert_eq!(
            fs::read(prepared.directory.join("bin/source.exe")).unwrap(),
            executable_fixture()
        );
        assert_eq!(prepared.digest.len(), 64);
        let collision = temp.path().join("collision.dglabplugin");
        let mut writer = zip::ZipWriter::new(File::create(&collision).unwrap());
        writer
            .start_file("A.txt", SimpleFileOptions::default())
            .unwrap();
        writer.write_all(b"a").unwrap();
        writer
            .start_file("a.txt", SimpleFileOptions::default())
            .unwrap();
        writer.write_all(b"b").unwrap();
        writer.finish().unwrap();
        assert_eq!(
            prepare_package(&collision, &temp.path().join("stage"))
                .err()
                .unwrap()
                .code,
            "invalid_package"
        );
    }
}
