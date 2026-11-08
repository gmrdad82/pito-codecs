# GPU routes, decode and encode

pcod tries the GPU first, then its own CPU code, for every decode and every encode, and says why on a `note` line whenever it falls back.

| Route | System | Decode | Encode |
| --- | --- | --- | --- |
| VA-API (Mesa, Intel iHD) | Linux | H.264, H.265, VP9, AV1 | H.264, H.265 |
| VideoToolbox | macOS | H.264, H.265, AV1 | H.264, H.265 |
| NVENC | Linux, Windows | none | H.264, H.265 |
| Media Foundation | Windows | H.264, H.265, AV1 | none |

Hardware actually exercised so far is AMD through VA-API; the others are built but less tested. Nothing extra is installed: libva loads at run time, and without a GPU everything runs on the CPU.

## decode and encode

```sh
pcod decode clip.mp4 --route gpu
pcod decode clip.mp4 --route cpu --threads 8
pcod encode clip.mp4 out.mp4 --codec hevc --route gpu
```

- `--route gpu` is the default for both. `--route cpu` forces the CPU; `--threads` caps CPU threads.
- `decode` decodes the whole video track and prints the route and fps. Without a GPU decoder for the codec it decodes on the CPU and says why.
- `encode` decodes, encodes and muxes an MP4, copying the audio. `--codec h264` (default) or `hevc`; `--crf` 23 by default. On the GPU, frames never leave the card.
- HEVC encodes on the GPU only. With no GPU HEVC encoder pcod falls back to H.264 on the GPU, then on the CPU, saying why.
- For file conversion with a quality choice, use `pcod in out --profile fast` ([`convert.md`](convert.md)), which is H.264 on the GPU.

## The 352x240 floor

VA-API H.264 and HEVC refuse pictures smaller than 352x240 with an error, because smaller ones reset AMD GPUs. Never work around it. Decode and encode such small pictures with `--route cpu`, and keep GPU test media at 352x240 or larger.

## Reporting speed

`decode` and `encode` print fps, but a busy machine reads slow. Quote a number only from a quiet one, and say which route produced it.
