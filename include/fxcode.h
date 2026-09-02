#ifndef FXCODE_H
#define FXCODE_H

#include <stddef.h>
#include <stdint.h>

typedef struct FxRuntime FxRuntime;
typedef struct FxNode FxNode;
typedef struct FxImage FxImage;
typedef struct FxVideo FxVideo;

typedef struct {
    int32_t code;
    char *message;
} FxResult;

typedef struct {
    uint32_t kitty_mode;
    uint32_t event_wait_ms;
} FxRuntimeConfig;

typedef struct {
    uint16_t cols;
    uint16_t rows;
    uint16_t pixel_width;
    uint16_t pixel_height;
} FxTerminalSize;

typedef struct {
    uint32_t kind;
    uint32_t key;
    uint32_t value;
    uint32_t mouse_kind;
    uint32_t mouse_button;
    uint16_t x;
    uint16_t y;
    FxTerminalSize size;
} FxEvent;

typedef struct {
    uint8_t has_width;
    uint8_t has_height;
    uint16_t width;
    uint16_t height;
    uint16_t x;
    uint16_t y;
    uint32_t padding_kind;
    uint8_t padding;
    uint8_t has_border;
    uint8_t has_background;
    uint8_t has_align_self;
    uint32_t border_color;
    uint32_t title_color;
    uint32_t background_color;
    const uint8_t *title;
    size_t title_len;
    uint32_t title_alignment;
    const FxVideo *media_video;
    uint16_t gap;
    uint32_t position;
    int16_t z;
    uint32_t align_items;
    uint32_t align_self;
    uint32_t flex_direction;
    uint32_t justify_content;
} FxStyle;

uint32_t fx_abi_version(void);
void fx_result_free(FxResult result);

FxResult fx_runtime_new(FxRuntimeConfig config, FxRuntime **out);
void fx_runtime_free(FxRuntime *runtime);
FxResult fx_runtime_size(const FxRuntime *runtime, FxTerminalSize *out);
FxResult fx_runtime_next_event(FxRuntime *runtime, FxEvent *out);
FxResult fx_runtime_draw(const FxRuntime *runtime, const FxNode *root);

FxResult fx_container_new(FxStyle style, FxNode **out);
FxResult fx_container_add(FxNode *parent, const FxNode *child);
void fx_node_free(FxNode *node);

FxResult fx_image_from_path(const uint8_t *path, size_t path_len, FxImage **out);
FxResult fx_image_from_rgba(uint32_t width, uint32_t height, const uint8_t *pixels,
                            size_t pixels_len, FxImage **out);
FxResult fx_image_from_png(const uint8_t *data, size_t data_len, FxImage **out);
FxResult fx_image_node(const FxImage *image, FxStyle style, uint32_t fit, FxNode **out);
void fx_image_free(FxImage *image);

FxResult fx_video_from_path(const uint8_t *path, size_t path_len, FxVideo **out);
FxResult fx_video_node(const FxVideo *video, FxStyle style, uint32_t fit, FxNode **out);
void fx_video_free(FxVideo *video);
FxResult fx_video_play(const FxVideo *video);
FxResult fx_video_pause(const FxVideo *video);
FxResult fx_video_toggle_pause(const FxVideo *video);
FxResult fx_video_seek_forward(const FxVideo *video);
FxResult fx_video_seek_backward(const FxVideo *video);
FxResult fx_video_set_volume(const FxVideo *video, uint8_t volume);
FxResult fx_video_seek_to(const FxVideo *video, double position);
FxResult fx_video_state(const FxVideo *video, uint8_t *paused, uint8_t *volume,
                        double *position, double *duration);

#endif
