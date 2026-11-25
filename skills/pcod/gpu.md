# GPU routes, decode and encode

Every decode and encode tries the GPU first and then pcod's own CPU code. When the GPU cannot take the job, pcod falls back and says why on a `note` line; the job still runs.

| Route | System | Decode | Encode |
| --- | --- | --- | --- |
| VA-API (Mesa, Intel iHD) | Linux | H.264, H.265, VP9, AV1 | H.264, H.265 |
| VideoToolbox | macOS | H.264, H.265, AV1 | H.264, H.265 |
| NVENC | Linux, Windows | none | H.264, H.265 |
| Media Foundation | Windows | H.264, H.265, AV1 | none |

Nothing extra is installed: libva loads at run time, and without a GPU everything runs on the CPU. VA-API on AMD is the route run before every release. VideoToolbox has run on one Apple M1 Max.

```sh
pcod decode clip.mp4
pcod decode clip.mp4 --route cpu --threads 4
pcod encode clip.mp4 out-h264.mp4 --route cpu
pcod encode clip.mp4 out-hevc.mp4 --codec hevc
```

- `decode` decodes the whole video track and prints `route` and `decoded N frames in S s: N fps`. `--route gpu` is the default; `--route cpu` forces the CPU and `--threads` caps its threads.
- `encode` decodes, encodes and muxes an MP4, copying the audio. `--codec h264` (the default) or `hevc`; `--crf` is 23 by default. On the GPU, frames stay on the card from decode to encode. It prints `decode`, `encode` and `encoded N frames`.
- HEVC encodes on the GPU only. With no GPU HEVC encoder, `encode` and `frames` write H.264 instead, on the GPU and then the CPU, and a `note` says why: `--codec hevc --route cpu` writes H.264.
- For a file with a quality choice, use convert: `pcod in.mp4 out.mp4 --profile fast` is H.264 on the GPU ([`convert.md`](convert.md)).
- `stills`, `filmstrip` and `frames` take `--route cpu` too.

## The 352x240 floor

VA-API H.264 and HEVC take pictures from 352x240; smaller ones faulted AMD's video engine. Below that pcod does not try the card: `decode`, `encode` and `--profile fast` run on the CPU with a `note` quoting the floor. A 320x180 clip decodes on the CPU whatever `--route` says.

## Speed

`decode` and `encode` print the speed of one run. A busy machine reads low, so say whether it was busy and which route produced the number before quoting it.
