#include <dlfcn.h>
#include <ffnvcodec/dynlink_cuda.h>
#include <ffnvcodec/nvEncodeAPI.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

typedef struct {
    void *cuda;
    void *library;
    tcuCtxDestroy_v2 *destroy_context;
    tcuCtxPushCurrent_v2 *push_context;
    tcuCtxPopCurrent_v2 *pop_context;
    CUcontext context;
    NV_ENCODE_API_FUNCTION_LIST api;
    void *session;
    NV_ENC_INPUT_PTR input;
    NV_ENC_OUTPUT_PTR output;
    NV_ENC_CONFIG config;
    NV_ENC_INITIALIZE_PARAMS params;
} NanoNvenc;

typedef void (*NanoNvencOutput)(void *, const uint8_t *, size_t);

static int fail(char *error, size_t len, const char *message, int code) {
    snprintf(error, len, "%s (%d)", message, code);
    return -1;
}

static void destroy_session(NanoNvenc *h) {
    if (h->output)
        h->api.nvEncDestroyBitstreamBuffer(h->session, h->output);
    if (h->input)
        h->api.nvEncDestroyInputBuffer(h->session, h->input);
    if (h->session)
        h->api.nvEncDestroyEncoder(h->session);
    h->output = NULL;
    h->input = NULL;
    h->session = NULL;
}

void nanoctl_nvenc_close(NanoNvenc *h) {
    if (!h)
        return;
    if (h->context) {
        CUcontext context;
        if (h->push_context(h->context) == CUDA_SUCCESS) {
            destroy_session(h);
            h->pop_context(&context);
        }
        h->destroy_context(h->context);
    }
    if (h->library)
        dlclose(h->library);
    if (h->cuda)
        dlclose(h->cuda);
    free(h);
}

static void rate_control(NV_ENC_RC_PARAMS *rc, uint32_t bitrate, uint32_t fps) {
    /* VBR lets easy frames undershoot the target instead of padding to it;
     * maxBitRate still caps the peak at the negotiated bitrate. */
    rc->rateControlMode = NV_ENC_PARAMS_RC_VBR;
    rc->averageBitRate = bitrate;
    rc->maxBitRate = bitrate;
    rc->vbvBufferSize = bitrate / fps + (bitrate % fps != 0);
    rc->vbvInitialDelay = rc->vbvBufferSize;
}

enum { NANO_CODEC_H264 = 0, NANO_CODEC_HEVC = 1, NANO_CODEC_AV1 = 2 };

static int codec_guid(uint32_t codec, GUID *guid) {
    switch (codec) {
    case NANO_CODEC_H264:
        *guid = NV_ENC_CODEC_H264_GUID;
        return 0;
    case NANO_CODEC_HEVC:
        *guid = NV_ENC_CODEC_HEVC_GUID;
        return 0;
#if NVENCAPI_MAJOR_VERSION >= 12
    case NANO_CODEC_AV1:
        *guid = NV_ENC_CODEC_AV1_GUID;
        return 0;
#endif
    default:
        return -1;
    }
}

static int supports_codec(NanoNvenc *h, GUID guid) {
    uint32_t count = 0;
    if (h->api.nvEncGetEncodeGUIDCount(h->session, &count) != NV_ENC_SUCCESS || count == 0 ||
        count > 64)
        return 0;
    GUID guids[64];
    uint32_t returned = 0;
    if (h->api.nvEncGetEncodeGUIDs(h->session, guids, count, &returned) != NV_ENC_SUCCESS)
        return 0;
    for (uint32_t i = 0; i < returned; i++)
        if (memcmp(&guids[i], &guid, sizeof(guid)) == 0)
            return 1;
    return 0;
}

static int initialize_session(NanoNvenc *h, uint32_t codec, uint32_t h264_high, uint32_t width, uint32_t height,
                              uint32_t bitrate, uint32_t fps, uint32_t quality,
                              char *error, size_t len) {
    GUID codec_id;
    if (codec_guid(codec, &codec_id) != 0)
        return fail(error, len, "this build's NVENC SDK headers do not support the codec", 0);
    NV_ENC_OPEN_ENCODE_SESSION_EX_PARAMS open = {0};
    open.version = NV_ENC_OPEN_ENCODE_SESSION_EX_PARAMS_VER;
    open.apiVersion = NVENCAPI_VERSION;
    open.device = h->context;
    open.deviceType = NV_ENC_DEVICE_TYPE_CUDA;
    NVENCSTATUS status = h->api.nvEncOpenEncodeSessionEx(&open, &h->session);
    if (status != NV_ENC_SUCCESS)
        return fail(error, len, "opening NVENC CUDA session failed", status);
    if (!supports_codec(h, codec_id))
        return fail(error, len, "this NVIDIA GPU cannot encode the requested codec", 0);

    GUID preset = quality == 0 ? NV_ENC_PRESET_P1_GUID :
                  quality == 2 ? NV_ENC_PRESET_P4_GUID : NV_ENC_PRESET_P3_GUID;
    NV_ENC_PRESET_CONFIG defaults = {0};
    defaults.version = NV_ENC_PRESET_CONFIG_VER;
    defaults.presetCfg.version = NV_ENC_CONFIG_VER;
    status = h->api.nvEncGetEncodePresetConfigEx(
        h->session, codec_id, preset,
        NV_ENC_TUNING_INFO_ULTRA_LOW_LATENCY, &defaults);
    if (status != NV_ENC_SUCCESS)
        return fail(error, len, "querying low-latency NVENC preset failed", status);

    h->config = defaults.presetCfg;
    /* Unrepairable IDRs already force a keyframe through the RTCP loss path,
     * so periodic IDRs are pure overhead. */
    h->config.gopLength = NVENC_INFINITE_GOPLENGTH;
    h->config.frameIntervalP = 1;
    h->config.rcParams.enableLookahead = 0;
    h->config.rcParams.enableTemporalAQ = 0;
    h->config.rcParams.zeroReorderDelay = 1;
    /* Quarter-resolution first pass (Sunshine's default): better rate allocation within the
     * one-frame VBV at a negligible latency cost. Falls back to single pass if unsupported. */
    h->config.rcParams.multiPass = NV_ENC_TWO_PASS_QUARTER_RESOLUTION;
    rate_control(&h->config.rcParams, bitrate, fps);
    if (codec == NANO_CODEC_H264) {
        /* High with frameIntervalP = 1 is Constrained High: CABAC and 8x8 transforms, no
         * B-frames. Baseline/CAVLC only when the browser did not offer High. */
        h->config.profileGUID =
            h264_high ? NV_ENC_H264_PROFILE_HIGH_GUID : NV_ENC_H264_PROFILE_BASELINE_GUID;
        NV_ENC_CONFIG_H264 *h264 = &h->config.encodeCodecConfig.h264Config;
        h264->idrPeriod = h->config.gopLength;
        h264->chromaFormatIDC = 1;
        h264->repeatSPSPPS = 1;
        h264->disableSPSPPS = 0;
        h264->entropyCodingMode = h264_high ? NV_ENC_H264_ENTROPY_CODING_MODE_CABAC
                                            : NV_ENC_H264_ENTROPY_CODING_MODE_CAVLC;
    } else if (codec == NANO_CODEC_HEVC) {
        h->config.profileGUID = NV_ENC_HEVC_PROFILE_MAIN_GUID;
        NV_ENC_CONFIG_HEVC *hevc = &h->config.encodeCodecConfig.hevcConfig;
        hevc->idrPeriod = h->config.gopLength;
        hevc->chromaFormatIDC = 1;
        hevc->repeatSPSPPS = 1;
        hevc->disableSPSPPS = 0;
    }
#if NVENCAPI_MAJOR_VERSION >= 12
    else if (codec == NANO_CODEC_AV1) {
        h->config.profileGUID = NV_ENC_AV1_PROFILE_MAIN_GUID;
        NV_ENC_CONFIG_AV1 *av1 = &h->config.encodeCodecConfig.av1Config;
        av1->idrPeriod = h->config.gopLength;
        av1->chromaFormatIDC = 1;
        av1->repeatSeqHdr = 1;
        av1->disableSeqHdr = 0;
        // WebRTC carries low-overhead OBUs (RFC AV1 RTP payload), not Annex-B temporal units.
        av1->outputAnnexBFormat = 0;
    }
#endif

    h->params = (NV_ENC_INITIALIZE_PARAMS){0};
    h->params.version = NV_ENC_INITIALIZE_PARAMS_VER;
    h->params.encodeGUID = codec_id;
    h->params.presetGUID = preset;
    h->params.tuningInfo = NV_ENC_TUNING_INFO_ULTRA_LOW_LATENCY;
    h->params.encodeWidth = h->params.darWidth = width;
    h->params.encodeHeight = h->params.darHeight = height;
    h->params.frameRateNum = fps;
    h->params.frameRateDen = 1;
    h->params.enablePTD = 1;
    h->params.encodeConfig = &h->config;
    status = h->api.nvEncInitializeEncoder(h->session, &h->params);
    if (status != NV_ENC_SUCCESS) {
        h->config.rcParams.multiPass = NV_ENC_MULTI_PASS_DISABLED;
        status = h->api.nvEncInitializeEncoder(h->session, &h->params);
    }
    if (status != NV_ENC_SUCCESS)
        return fail(error, len, "initializing NVENC encoder failed", status);

    NV_ENC_CREATE_INPUT_BUFFER input = {0};
    input.version = NV_ENC_CREATE_INPUT_BUFFER_VER;
    input.width = width;
    input.height = height;
    // NVENC names this word format ABGR; on little-endian Linux the bytes are RGBA.
    input.bufferFmt = NV_ENC_BUFFER_FORMAT_ABGR;
    status = h->api.nvEncCreateInputBuffer(h->session, &input);
    if (status != NV_ENC_SUCCESS)
        return fail(error, len, "allocating NVENC RGBA input failed", status);
    h->input = input.inputBuffer;

    NV_ENC_CREATE_BITSTREAM_BUFFER output = {0};
    output.version = NV_ENC_CREATE_BITSTREAM_BUFFER_VER;
    status = h->api.nvEncCreateBitstreamBuffer(h->session, &output);
    if (status != NV_ENC_SUCCESS)
        return fail(error, len, "allocating NVENC bitstream buffer failed", status);
    h->output = output.bitstreamBuffer;
    return 0;
}

NanoNvenc *nanoctl_nvenc_open(uint32_t codec, uint32_t h264_high, uint32_t width, uint32_t height, uint32_t bitrate,
                             uint32_t fps, uint32_t quality, char *error, size_t len) {
    NanoNvenc *h = calloc(1, sizeof(*h));
    if (!h) {
        fail(error, len, "allocating NVENC state failed", 0);
        return NULL;
    }
    h->library = dlopen("libnvidia-encode.so.1", RTLD_NOW | RTLD_LOCAL);
    h->cuda = dlopen("libcuda.so.1", RTLD_NOW | RTLD_LOCAL);
    if (!h->library || !h->cuda) {
        fail(error, len, "NVIDIA driver libraries are missing", 0);
        goto failed;
    }

    tcuInit *init = (tcuInit *)dlsym(h->cuda, "cuInit");
    tcuDeviceGetCount *count_devices = (tcuDeviceGetCount *)dlsym(h->cuda, "cuDeviceGetCount");
    tcuDeviceGet *get_device = (tcuDeviceGet *)dlsym(h->cuda, "cuDeviceGet");
    tcuCtxCreate_v2 *create_context = (tcuCtxCreate_v2 *)dlsym(h->cuda, "cuCtxCreate_v2");
    h->destroy_context = (tcuCtxDestroy_v2 *)dlsym(h->cuda, "cuCtxDestroy_v2");
    h->push_context = (tcuCtxPushCurrent_v2 *)dlsym(h->cuda, "cuCtxPushCurrent_v2");
    h->pop_context = (tcuCtxPopCurrent_v2 *)dlsym(h->cuda, "cuCtxPopCurrent_v2");
    typedef NVENCSTATUS (NVENCAPI *CreateApi)(NV_ENCODE_API_FUNCTION_LIST *);
    CreateApi create_api = (CreateApi)dlsym(h->library, "NvEncodeAPICreateInstance");
    typedef NVENCSTATUS (NVENCAPI *MaxVersion)(uint32_t *);
    MaxVersion max_version = (MaxVersion)dlsym(h->library, "NvEncodeAPIGetMaxSupportedVersion");
    if (!init || !count_devices || !get_device || !create_context || !h->destroy_context ||
        !h->push_context || !h->pop_context || !create_api || !max_version) {
        fail(error, len, "NVIDIA driver is missing required entry points", 0);
        goto failed;
    }
    uint32_t supported = 0;
    NVENCSTATUS status = max_version(&supported);
    if (status != NV_ENC_SUCCESS ||
        supported < ((NVENCAPI_MAJOR_VERSION << 4) | NVENCAPI_MINOR_VERSION)) {
        fail(error, len, "NVIDIA driver does not support the NVENC API used by this build", (int)supported);
        goto failed;
    }
    h->api.version = NV_ENCODE_API_FUNCTION_LIST_VER;
    status = create_api(&h->api);
    if (status != NV_ENC_SUCCESS) {
        fail(error, len, "loading NVENC API failed", status);
        goto failed;
    }

    CUresult cuda_status = init(0);
    int count = 0;
    if (cuda_status == CUDA_SUCCESS)
        cuda_status = count_devices(&count);
    if (cuda_status != CUDA_SUCCESS || count < 1) {
        fail(error, len, "no usable NVIDIA CUDA device", cuda_status);
        goto failed;
    }
    for (int index = 0; index < count; index++) {
        CUdevice device;
        CUcontext context;
        cuda_status = get_device(&device, index);
        if (cuda_status == CUDA_SUCCESS)
            cuda_status = create_context(&context, 0, device);
        if (cuda_status != CUDA_SUCCESS) {
            fail(error, len, "creating NVIDIA CUDA context failed", cuda_status);
            continue;
        }
        h->context = context;
        int initialized = initialize_session(h, codec, h264_high, width, height, bitrate, fps, quality, error, len);
        if (initialized != 0)
            destroy_session(h);
        CUcontext popped;
        cuda_status = h->pop_context(&popped);
        if (initialized == 0 && cuda_status == CUDA_SUCCESS)
            return h;
        if (cuda_status != CUDA_SUCCESS) {
            destroy_session(h);
            fail(error, len, "restoring CUDA context failed", cuda_status);
        }
        h->destroy_context(context);
        h->context = NULL;
    }
failed:
    nanoctl_nvenc_close(h);
    return NULL;
}

int nanoctl_nvenc_set_bitrate(NanoNvenc *h, uint32_t bitrate, char *error, size_t len) {
    CUresult status = h->push_context(h->context);
    if (status != CUDA_SUCCESS)
        return fail(error, len, "entering NVENC CUDA context failed", status);
    NV_ENC_CONFIG config = h->config;
    rate_control(&config.rcParams, bitrate, h->params.frameRateNum);
    NV_ENC_RECONFIGURE_PARAMS update = {0};
    update.version = NV_ENC_RECONFIGURE_PARAMS_VER;
    update.reInitEncodeParams = h->params;
    update.reInitEncodeParams.encodeConfig = &config;
    NVENCSTATUS encoded = h->api.nvEncReconfigureEncoder(h->session, &update);
    if (encoded == NV_ENC_SUCCESS)
        h->config = config;
    CUcontext popped;
    status = h->pop_context(&popped);
    if (status != CUDA_SUCCESS)
        return fail(error, len, "restoring CUDA context failed", status);
    return encoded == NV_ENC_SUCCESS ? 0 : fail(error, len, "reconfiguring NVENC bitrate failed", encoded);
}

static int encode_current(NanoNvenc *h, const uint8_t *rgba, uint64_t timestamp,
                          bool keyframe, NanoNvencOutput callback, void *destination,
                          char *error, size_t len) {
    NV_ENC_LOCK_INPUT_BUFFER lock = {0};
    lock.version = NV_ENC_LOCK_INPUT_BUFFER_VER;
    lock.inputBuffer = h->input;
    NVENCSTATUS status = h->api.nvEncLockInputBuffer(h->session, &lock);
    if (status != NV_ENC_SUCCESS)
        return fail(error, len, "locking NVENC input failed", status);
    size_t row_size = (size_t)h->params.encodeWidth * 4;
    if (!lock.bufferDataPtr || lock.pitch < row_size) {
        h->api.nvEncUnlockInputBuffer(h->session, h->input);
        return fail(error, len, "NVENC returned an invalid input buffer", 0);
    }
    for (size_t row = 0; row < h->params.encodeHeight; row++)
        memcpy((uint8_t *)lock.bufferDataPtr + row * lock.pitch, rgba + row * row_size, row_size);
    status = h->api.nvEncUnlockInputBuffer(h->session, h->input);
    if (status != NV_ENC_SUCCESS)
        return fail(error, len, "unlocking NVENC input failed", status);

    NV_ENC_PIC_PARAMS frame = {0};
    frame.version = NV_ENC_PIC_PARAMS_VER;
    frame.inputWidth = h->params.encodeWidth;
    frame.inputHeight = h->params.encodeHeight;
    frame.inputPitch = lock.pitch;
    frame.inputBuffer = h->input;
    frame.bufferFmt = NV_ENC_BUFFER_FORMAT_ABGR;
    frame.outputBitstream = h->output;
    frame.pictureStruct = NV_ENC_PIC_STRUCT_FRAME;
    frame.inputTimeStamp = timestamp;
    if (keyframe)
        frame.encodePicFlags = NV_ENC_PIC_FLAG_FORCEIDR | NV_ENC_PIC_FLAG_OUTPUT_SPSPPS;
    status = h->api.nvEncEncodePicture(h->session, &frame);
    if (status != NV_ENC_SUCCESS)
        return fail(error, len, "submitting frame to NVENC failed", status);

    NV_ENC_LOCK_BITSTREAM output = {0};
    output.version = NV_ENC_LOCK_BITSTREAM_VER;
    output.outputBitstream = h->output;
    status = h->api.nvEncLockBitstream(h->session, &output);
    if (status != NV_ENC_SUCCESS)
        return fail(error, len, "locking NVENC bitstream failed", status);
    bool valid = output.bitstreamBufferPtr && output.bitstreamSizeInBytes &&
                 output.bitstreamSizeInBytes <= 32 * 1024 * 1024;
    if (valid)
        callback(destination, output.bitstreamBufferPtr, output.bitstreamSizeInBytes);
    status = h->api.nvEncUnlockBitstream(h->session, h->output);
    if (status != NV_ENC_SUCCESS)
        return fail(error, len, "unlocking NVENC bitstream failed", status);
    return valid ? 0 : fail(error, len, "NVENC returned an invalid access unit", 0);
}

int nanoctl_nvenc_encode(NanoNvenc *h, const uint8_t *rgba, uint64_t timestamp,
                         bool keyframe, NanoNvencOutput callback, void *destination,
                         char *error, size_t len) {
    CUresult status = h->push_context(h->context);
    if (status != CUDA_SUCCESS)
        return fail(error, len, "entering NVENC CUDA context failed", status);
    int result = encode_current(h, rgba, timestamp, keyframe, callback, destination, error, len);
    CUcontext popped;
    status = h->pop_context(&popped);
    if (status != CUDA_SUCCESS)
        return fail(error, len, "restoring CUDA context failed", status);
    return result;
}
