#include <stdint.h>
#include <stdlib.h>
#include <string.h>
#include <x264.h>

#ifdef _WIN32
#include <windows.h>
typedef HMODULE Library;
static Library load(const char *name) {
    return LoadLibraryExA(name, NULL, LOAD_LIBRARY_SEARCH_DEFAULT_DIRS);
}
static void unload(Library library) { FreeLibrary(library); }
static void *symbol(Library library, const char *name) {
    return (void *)GetProcAddress(library, name);
}
#else
#include <dlfcn.h>
typedef void *Library;
static Library load(const char *name) { return dlopen(name, RTLD_NOW | RTLD_LOCAL); }
static void unload(Library library) { dlclose(library); }
static void *symbol(Library library, const char *name) { return dlsym(library, name); }
#endif

#define STRING_(value) #value
#define STRING(value) STRING_(value)

typedef struct {
    Library library;
    x264_t *encoder;
    int (*param_default_preset)(x264_param_t *, const char *, const char *);
    int (*param_apply_profile)(x264_param_t *, const char *);
    x264_t *(*encoder_open)(x264_param_t *);
    void (*encoder_close)(x264_t *);
    void (*encoder_parameters)(x264_t *, x264_param_t *);
    int (*encoder_reconfig)(x264_t *, x264_param_t *);
    int (*encoder_encode)(x264_t *, x264_nal_t **, int *, x264_picture_t *, x264_picture_t *);
    void (*picture_init)(x264_picture_t *);
} NanoX264;

void nano_x264_close(NanoX264 *context) {
    if (!context) return;
    if (context->encoder) context->encoder_close(context->encoder);
    if (context->library) unload(context->library);
    free(context);
}

/* Only our scalar/opaque ABI crosses into Rust; x264 structs stay beside their headers. */
#define RESOLVE(field, name) do { \
    void *address = symbol(context->library, name); \
    if (!address || sizeof(context->field) != sizeof(address)) goto fail; \
    memcpy(&context->field, &address, sizeof(context->field)); \
} while (0)

static int buffer_size(int kbps, int fps, int frames) {
    return (int)(((int64_t)kbps * frames + fps - 1) / fps);
}

NanoX264 *nano_x264_open(int width, int height, int kbps, int fps, int frames,
                       const char *library) {
    if (width < 2 || height < 2 || width % 2 || height % 2 ||
        kbps <= 0 || fps <= 0 || frames <= 0) return NULL;
    NanoX264 *context = calloc(1, sizeof(*context));
    if (!context) return NULL;
    if (library) {
        context->library = load(library);
    } else {
        static const char *names[] = {
#ifdef _WIN32
            "libx264-" STRING(X264_BUILD) ".dll", "libx264.dll", "x264.dll",
#elif defined(__APPLE__)
            "libx264." STRING(X264_BUILD) ".dylib", "libx264.dylib",
#else
            "libx264.so." STRING(X264_BUILD), "libx264.so",
#endif
        };
        for (size_t i = 0; i < sizeof(names) / sizeof(names[0]); ++i) {
            context->library = load(names[i]);
            if (context->library) break;
        }
    }
    if (!context->library) goto fail;
    RESOLVE(param_default_preset, "x264_param_default_preset");
    RESOLVE(param_apply_profile, "x264_param_apply_profile");
    RESOLVE(encoder_open, "x264_encoder_open_" STRING(X264_BUILD));
    RESOLVE(encoder_close, "x264_encoder_close");
    RESOLVE(encoder_parameters, "x264_encoder_parameters");
    RESOLVE(encoder_reconfig, "x264_encoder_reconfig");
    RESOLVE(encoder_encode, "x264_encoder_encode");
    RESOLVE(picture_init, "x264_picture_init");
    x264_param_t p;
    const char *preset = getenv("NANOCTL_X264_PRESET");
    if (!preset || !preset[0]) preset = "veryfast";
    const char *tune = getenv("NANOCTL_X264_TUNE");
    if (!tune || !tune[0]) tune = "zerolatency";
    if (context->param_default_preset(&p, preset, tune) < 0) goto fail;
    p.i_width = width;
    p.i_height = height;
    p.i_csp = X264_CSP_I420;
    p.i_bitdepth = 8;
    const char *threads = getenv("NANOCTL_X264_THREADS");
    p.i_threads = threads && threads[0] ? atoi(threads) : 2;
    p.b_sliced_threads = 1;
    p.i_fps_num = fps;
    p.i_fps_den = 1;
    p.i_timebase_num = 1;
    p.i_timebase_den = 1000000;
    p.b_vfr_input = 0;
    p.i_bframe = 0;
    p.i_keyint_max = X264_KEYINT_MAX_INFINITE;
    p.i_scenecut_threshold = 0;
    const char *me = getenv("NANOCTL_X264_ME");
    if (me && me[0]) {
        p.analyse.i_me_method = !strcmp(me, "dia")   ? X264_ME_DIA
                              : !strcmp(me, "hex")   ? X264_ME_HEX
                              : !strcmp(me, "umh")   ? X264_ME_UMH
                              : !strcmp(me, "esa")   ? X264_ME_ESA
                              : !strcmp(me, "tesa")  ? X264_ME_TESA
                                                   : p.analyse.i_me_method;
    }
    const char *subme = getenv("NANOCTL_X264_SUBME");
    if (subme && subme[0]) p.analyse.i_subpel_refine = atoi(subme);
    p.i_sync_lookahead = 0;
    p.rc.i_lookahead = 0;
    p.rc.i_rc_method = X264_RC_CRF;
    const char *crf = getenv("NANOCTL_X264_CRF");
    p.rc.f_rf_constant = crf && crf[0] ? (float)atof(crf) : 18.0f;
    p.rc.i_qp_min = 0;
    p.rc.i_qp_max = 51;
    p.rc.i_bitrate = kbps;
    p.rc.i_vbv_max_bitrate = kbps;
    p.rc.i_vbv_buffer_size = buffer_size(kbps, fps, frames);
    p.b_annexb = 1;
    p.b_repeat_headers = 1;
    p.vui.b_fullrange = 0;
    p.vui.i_colorprim = 1;
    p.vui.i_transfer = 13;
    p.vui.i_colmatrix = 6;
    p.vui.i_chroma_loc = 1;
    p.i_log_level = X264_LOG_WARNING;
    const char *profile = getenv("NANOCTL_X264_PROFILE");
    if (!profile || !profile[0]) profile = "baseline";
    if (context->param_apply_profile(&p, profile) < 0) goto fail;
    context->encoder = context->encoder_open(&p);
    if (!context->encoder) goto fail;
    return context;
fail:
    nano_x264_close(context);
    return NULL;
}

int nano_x264_bitrate(NanoX264 *context, int kbps, int fps, int frames) {
    if (kbps <= 0 || fps <= 0 || frames <= 0) return -1;
    x264_param_t p;
    context->encoder_parameters(context->encoder, &p);
    p.rc.i_bitrate = kbps;
    p.rc.i_vbv_max_bitrate = kbps;
    p.rc.i_vbv_buffer_size = buffer_size(kbps, fps, frames);
    return context->encoder_reconfig(context->encoder, &p);
}

int nano_x264_encode(NanoX264 *context, const uint8_t *y, const uint8_t *u,
                     const uint8_t *v, int width, int64_t pts, int keyframe,
                     const uint8_t **bytes) {
    x264_picture_t input, output;
    context->picture_init(&input);
    input.img.i_csp = X264_CSP_I420;
    input.img.i_plane = 3;
    input.img.i_stride[0] = width;
    input.img.i_stride[1] = input.img.i_stride[2] = width / 2;
    input.img.plane[0] = (uint8_t *)y;
    input.img.plane[1] = (uint8_t *)u;
    input.img.plane[2] = (uint8_t *)v;
    input.i_pts = pts;
    input.i_type = keyframe ? X264_TYPE_IDR : X264_TYPE_AUTO;
    x264_nal_t *nals = NULL;
    int count = 0;
    int length = context->encoder_encode(context->encoder, &nals, &count, &input, &output);
    *bytes = length > 0 && count > 0 ? nals[0].p_payload : NULL;
    return length;
}

typedef struct {
    int bitrate, buffer_kbit, crf_milli, qp_max, csp, threads, bframes, lookahead;
    int sync_lookahead, cabac, transform8x8, repeat_headers, annexb, fullrange;
    int primaries, transfer, matrix, chroma_location, keyint, rc_method;
} NanoX264Params;

void nano_x264_parameters(NanoX264 *context, NanoX264Params *out) {
    x264_param_t p;
    context->encoder_parameters(context->encoder, &p);
    *out = (NanoX264Params){
        p.rc.i_vbv_max_bitrate, p.rc.i_vbv_buffer_size, (int)(p.rc.f_rf_constant * 1000),
        p.rc.i_qp_max, p.i_csp, p.i_threads, p.i_bframe, p.rc.i_lookahead,
        p.i_sync_lookahead, p.b_cabac, p.analyse.b_transform_8x8, p.b_repeat_headers,
        p.b_annexb, p.vui.b_fullrange, p.vui.i_colorprim, p.vui.i_transfer,
        p.vui.i_colmatrix, p.vui.i_chroma_loc, p.i_keyint_max, p.rc.i_rc_method
    };
}
