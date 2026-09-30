$ErrorActionPreference = 'Stop'

# 合成素材仅用于测试；运行应用和 cargo test 均不需要 FFmpeg。
$videoInput = @('-f', 'lavfi', '-i', 'color=c=black:s=32x32:r=10:d=0.5')
$toneInput = @('-f', 'lavfi', '-i', 'aevalsrc=0.25*sin(2*PI*440*t)|0.25*sin(2*PI*880*t):s=48000:d=0.5')
$constantInput = @('-f', 'lavfi', '-i', 'aevalsrc=0.25|-0.5:s=48000:d=0.5')
$common = @('-hide_banner', '-loglevel', 'error', '-y')

& ffmpeg @common @videoInput @toneInput -map 0:v -map 1:a -c:v libx264 -pix_fmt yuv420p -c:a aac -movflags +faststart "$PSScriptRoot/video-aac.mp4"
if ($LASTEXITCODE -ne 0) { throw '生成 MP4 失败' }
& ffmpeg @common @videoInput @constantInput -map 0:v -map 1:a -c:v libx264 -pix_fmt yuv420p -c:a pcm_s16le "$PSScriptRoot/video-pcm.mov"
if ($LASTEXITCODE -ne 0) { throw '生成 MOV 失败' }
& ffmpeg @common @videoInput @toneInput @constantInput -map 0:v -map 1:a -map 2:a -c:v libx264 -pix_fmt yuv420p -c:a:0 ac3 -c:a:1 pcm_s16le -disposition:a:0 default -disposition:a:1 0 "$PSScriptRoot/video-fallback.mkv"
if ($LASTEXITCODE -ne 0) { throw '生成多音轨 MKV 失败' }
& ffmpeg @common @videoInput @toneInput -map 0:v -map 1:a -c:v libvpx -c:a libvorbis "$PSScriptRoot/video-vorbis.webm"
if ($LASTEXITCODE -ne 0) { throw '生成 Vorbis WebM 失败' }
& ffmpeg @common @videoInput @toneInput -map 0:v -map 1:a -c:v libvpx -c:a libopus "$PSScriptRoot/video-opus.webm"
if ($LASTEXITCODE -ne 0) { throw '生成 Opus WebM 失败' }
& ffmpeg @common @videoInput -map 0:v -c:v libx264 -pix_fmt yuv420p -an -movflags +faststart "$PSScriptRoot/video-no-audio.mp4"
if ($LASTEXITCODE -ne 0) { throw '生成无音轨 MP4 失败' }
