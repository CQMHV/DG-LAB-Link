# 视频音轨测试素材

所有素材均为 `generate.ps1` 本地生成的半秒黑色视频与合成声音，不包含外部媒体。文件随仓库保存，应用和测试运行均不依赖 FFmpeg；重新生成需要安装 FFmpeg。

- `video-aac.mp4`：H.264 视频 + AAC 立体声音轨，左声道 440 Hz、右声道 880 Hz。
- `video-pcm.mov`：H.264 视频 + PCM 立体声音轨，左声道 0.25、右声道 -0.5。
- `video-fallback.mkv`：H.264 视频、默认 AC-3 音轨和第二条 PCM 音轨，验证自动回退到可解码音轨。
- `video-vorbis.webm`：VP8 视频 + Vorbis 立体声音轨，左右频率同 MP4。
- `video-opus.webm`：VP8 视频 + 暂不支持的 Opus 音轨，验证可见错误。
- `video-no-audio.mp4`：仅含 H.264 视频，验证无音轨错误。

在仓库根目录运行：

```powershell
& ./crates/builtin-plugins/src/sources/audio/testdata/generate.ps1
```
