use super::ffi::*;
use std::mem::{offset_of, size_of};

#[test]
fn layouts_match_the_sdk_header() {
    let sizes = [
        (size_of::<NV_ENC_OPEN_ENCODE_SESSION_EX_PARAMS>(), 1552),
        (size_of::<NV_ENC_CAPS_PARAM>(), 256),
        (size_of::<NV_ENC_RC_PARAMS>(), 128),
        (size_of::<NV_ENC_CONFIG_H264_VUI_PARAMETERS>(), 112),
        (size_of::<NV_ENC_CONFIG_H264>(), 1792),
        (size_of::<NV_ENC_CONFIG_HEVC>(), 1560),
        (size_of::<NV_ENC_CODEC_CONFIG>(), 1792),
        (size_of::<NV_ENC_CONFIG>(), 3584),
        (size_of::<NV_ENC_PRESET_CONFIG>(), 5128),
        (size_of::<NV_ENC_INITIALIZE_PARAMS>(), 1808),
        (size_of::<NV_ENC_CREATE_INPUT_BUFFER>(), 776),
        (size_of::<NV_ENC_CREATE_BITSTREAM_BUFFER>(), 776),
        (size_of::<NV_ENC_CODEC_PIC_PARAMS>(), 1552),
        (size_of::<NV_ENC_PIC_PARAMS>(), 3360),
        (size_of::<NV_ENC_LOCK_BITSTREAM>(), 1552),
        (size_of::<NV_ENC_LOCK_INPUT_BUFFER>(), 1544),
        (size_of::<NV_ENC_SEQUENCE_PARAM_PAYLOAD>(), 1544),
        (size_of::<NV_ENCODE_API_FUNCTION_LIST>(), 2552),
    ];
    for (i, (ours, theirs)) in sizes.into_iter().enumerate() {
        assert_eq!(ours, theirs, "structure {i}");
    }
    let offsets = [
        (
            offset_of!(NV_ENC_OPEN_ENCODE_SESSION_EX_PARAMS, apiVersion),
            24,
        ),
        (offset_of!(NV_ENC_RC_PARAMS, averageBitRate), 20),
        (offset_of!(NV_ENC_RC_PARAMS, flags), 36),
        (offset_of!(NV_ENC_RC_PARAMS, targetQuality), 88),
        (offset_of!(NV_ENC_RC_PARAMS, lookaheadDepth), 90),
        (offset_of!(NV_ENC_RC_PARAMS, multiPass), 100),
        (offset_of!(NV_ENC_CONFIG_H264, entropyCodingMode), 44),
        (offset_of!(NV_ENC_CONFIG_H264, maxNumRefFrames), 60),
        (offset_of!(NV_ENC_CONFIG_H264, h264VUIParameters), 72),
        (offset_of!(NV_ENC_CONFIG_H264, useBFramesAsRef), 200),
        (offset_of!(NV_ENC_CONFIG_HEVC, idrPeriod), 20),
        (offset_of!(NV_ENC_CONFIG_HEVC, maxNumRefFramesInDPB), 32),
        (offset_of!(NV_ENC_CONFIG_HEVC, useBFramesAsRef), 180),
        (offset_of!(NV_ENC_CONFIG, gopLength), 20),
        (offset_of!(NV_ENC_CONFIG, rcParams), 40),
        (offset_of!(NV_ENC_CONFIG, encodeCodecConfig), 168),
        (offset_of!(NV_ENC_PRESET_CONFIG, presetCfg), 8),
        (offset_of!(NV_ENC_INITIALIZE_PARAMS, enablePTD), 64),
        (offset_of!(NV_ENC_INITIALIZE_PARAMS, encodeConfig), 88),
        (offset_of!(NV_ENC_INITIALIZE_PARAMS, tuningInfo), 136),
        (offset_of!(NV_ENC_INITIALIZE_PARAMS, bufferFormat), 140),
        (offset_of!(NV_ENC_CREATE_INPUT_BUFFER, inputBuffer), 24),
        (
            offset_of!(NV_ENC_CREATE_BITSTREAM_BUFFER, bitstreamBuffer),
            16,
        ),
        (offset_of!(NV_ENC_PIC_PARAMS, inputTimeStamp), 24),
        (offset_of!(NV_ENC_PIC_PARAMS, inputBuffer), 40),
        (offset_of!(NV_ENC_PIC_PARAMS, bufferFmt), 64),
        (offset_of!(NV_ENC_PIC_PARAMS, codecPicParams), 80),
        (offset_of!(NV_ENC_LOCK_BITSTREAM, bitstreamSizeInBytes), 36),
        (offset_of!(NV_ENC_LOCK_BITSTREAM, outputTimeStamp), 40),
        (offset_of!(NV_ENC_LOCK_BITSTREAM, bitstreamBufferPtr), 56),
        (offset_of!(NV_ENC_LOCK_BITSTREAM, pictureType), 64),
        (offset_of!(NV_ENC_LOCK_INPUT_BUFFER, bufferDataPtr), 16),
        (offset_of!(NV_ENC_LOCK_INPUT_BUFFER, pitch), 24),
        (
            offset_of!(NV_ENC_SEQUENCE_PARAM_PAYLOAD, outSPSPPSPayloadSize),
            24,
        ),
        (
            offset_of!(NV_ENCODE_API_FUNCTION_LIST, nvEncGetEncodeCaps),
            64,
        ),
        (
            offset_of!(NV_ENCODE_API_FUNCTION_LIST, nvEncInitializeEncoder),
            96,
        ),
        (
            offset_of!(NV_ENCODE_API_FUNCTION_LIST, nvEncEncodePicture),
            136,
        ),
        (
            offset_of!(NV_ENCODE_API_FUNCTION_LIST, nvEncGetSequenceParams),
            184,
        ),
        (
            offset_of!(NV_ENCODE_API_FUNCTION_LIST, nvEncDestroyEncoder),
            224,
        ),
        (
            offset_of!(NV_ENCODE_API_FUNCTION_LIST, nvEncOpenEncodeSessionEx),
            240,
        ),
        (
            offset_of!(NV_ENCODE_API_FUNCTION_LIST, nvEncGetLastErrorString),
            304,
        ),
        (
            offset_of!(NV_ENCODE_API_FUNCTION_LIST, nvEncGetEncodePresetConfigEx),
            320,
        ),
    ];
    for (i, (ours, theirs)) in offsets.into_iter().enumerate() {
        assert_eq!(ours, theirs, "field {i}");
    }
}

#[test]
fn versions_and_guids_match_the_sdk_header() {
    assert_eq!(NVENCAPI_VERSION, 16777228);
    assert_eq!(NV_ENCODE_API_FUNCTION_LIST_VER, 1895956492);
    assert_eq!(NV_ENC_CONFIG_VER, 4043833356);
    assert_eq!(NV_ENC_INITIALIZE_PARAMS_VER, 4043702284);
    assert_eq!(NV_ENC_PRESET_CONFIG_VER, 4043571212);
    assert_eq!(NV_ENC_PIC_PARAMS_VER, 4043702284);
    assert_eq!(NV_ENC_LOCK_BITSTREAM_VER, 4043374604);
    assert_eq!(NV_ENC_LOCK_INPUT_BUFFER_VER, 1895890956);
    assert_eq!(H264_REPEAT_SPSPPS, 0x1000);
    assert_eq!(HEVC_PIXEL_BIT_DEPTH_MASK, 0x3800);
    assert_eq!(
        NV_ENC_CODEC_HEVC_GUID.to_u128(),
        0x790cdc8845224d7b9425bda9975f7603
    );
    assert_eq!(
        NV_ENC_HEVC_PROFILE_MAIN10_GUID.to_u128(),
        0xfa4d2b6c3a5b411a80180a3f5e3c9be5
    );
    assert_eq!(
        NV_ENC_PRESET_P4_GUID.to_u128(),
        0x90a7b826df064862b9d2cd6d73a08681
    );
}
