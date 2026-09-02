package fxcode

/*
#cgo CFLAGS: -I${SRCDIR}/../../include
#cgo LDFLAGS: -L${SRCDIR}/../../target/debug -lfxcode -lmpv -ldl -lpthread -lm
#include "fxcode.h"
*/
import "C"

import (
	"errors"
	"runtime"
	"unsafe"
)

type nativeRuntime struct{ ptr *C.FxRuntime }
type nativeNode struct{ ptr *C.FxNode }
type nativeImage struct{ ptr *C.FxImage }
type nativeVideo struct{ ptr *C.FxVideo }

func resultError(result C.FxResult) error {
	defer C.fx_result_free(result)
	if result.code == 0 {
		return nil
	}
	if result.message == nil {
		return errors.New("fxcode native call failed")
	}
	return errors.New(C.GoString(result.message))
}

func bytePointer(value []byte) *C.uint8_t {
	if len(value) == 0 {
		return nil
	}
	return (*C.uint8_t)(unsafe.Pointer(&value[0]))
}

func newNativeRuntime(config Config) (*nativeRuntime, error) {
	var pointer *C.FxRuntime
	result := C.fx_runtime_new(C.FxRuntimeConfig{
		kitty_mode:    C.uint32_t(config.Kitty),
		event_wait_ms: C.uint32_t(config.EventWaitMS),
	}, &pointer)
	if err := resultError(result); err != nil {
		return nil, err
	}
	return &nativeRuntime{ptr: pointer}, nil
}

func (value *nativeRuntime) free() {
	C.fx_runtime_free(value.ptr)
	value.ptr = nil
}

func (value *nativeRuntime) size() (TerminalSize, error) {
	var size C.FxTerminalSize
	if err := resultError(C.fx_runtime_size(value.ptr, &size)); err != nil {
		return TerminalSize{}, err
	}
	return terminalSize(size), nil
}

func (value *nativeRuntime) nextEvent() (Event, error) {
	var event C.FxEvent
	if err := resultError(C.fx_runtime_next_event(value.ptr, &event)); err != nil {
		return Event{}, err
	}
	return Event{
		Kind:        EventKind(event.kind),
		Key:         Key(event.key),
		Rune:        rune(event.value),
		Function:    uint8(event.value),
		MouseKind:   MouseKind(event.mouse_kind),
		MouseButton: MouseButton(event.mouse_button),
		X:           uint16(event.x),
		Y:           uint16(event.y),
		Size:        terminalSize(event.size),
	}, nil
}

func (value *nativeRuntime) draw(root *nativeNode) error {
	return resultError(C.fx_runtime_draw(value.ptr, root.ptr))
}

func terminalSize(value C.FxTerminalSize) TerminalSize {
	return TerminalSize{
		Cols:        uint16(value.cols),
		Rows:        uint16(value.rows),
		PixelWidth:  uint16(value.pixel_width),
		PixelHeight: uint16(value.pixel_height),
	}
}

func newNativeContainer(style Style) (*nativeNode, error) {
	value, title := nativeStyle(style)
	var pointer *C.FxNode
	result := C.fx_container_new(value, &pointer)
	runtime.KeepAlive(title)
	if err := resultError(result); err != nil {
		return nil, err
	}
	return &nativeNode{ptr: pointer}, nil
}

func (value *nativeNode) add(child *nativeNode) error {
	return resultError(C.fx_container_add(value.ptr, child.ptr))
}

func (value *nativeNode) free() {
	C.fx_node_free(value.ptr)
	value.ptr = nil
}

func newNativeImageFromPath(path string) (*nativeImage, error) {
	data := []byte(path)
	var pointer *C.FxImage
	result := C.fx_image_from_path(bytePointer(data), C.size_t(len(data)), &pointer)
	runtime.KeepAlive(data)
	if err := resultError(result); err != nil {
		return nil, err
	}
	return &nativeImage{ptr: pointer}, nil
}

func newNativeImageFromRGBA(width, height uint32, pixels []byte) (*nativeImage, error) {
	var pointer *C.FxImage
	result := C.fx_image_from_rgba(
		C.uint32_t(width), C.uint32_t(height), bytePointer(pixels), C.size_t(len(pixels)), &pointer,
	)
	runtime.KeepAlive(pixels)
	if err := resultError(result); err != nil {
		return nil, err
	}
	return &nativeImage{ptr: pointer}, nil
}

func newNativeImageFromPNG(data []byte) (*nativeImage, error) {
	var pointer *C.FxImage
	result := C.fx_image_from_png(bytePointer(data), C.size_t(len(data)), &pointer)
	runtime.KeepAlive(data)
	if err := resultError(result); err != nil {
		return nil, err
	}
	return &nativeImage{ptr: pointer}, nil
}

func (value *nativeImage) node(style Style, fit ImageFit) (*nativeNode, error) {
	nativeStyle, title := nativeStyle(style)
	var pointer *C.FxNode
	result := C.fx_image_node(value.ptr, nativeStyle, C.uint32_t(fit), &pointer)
	runtime.KeepAlive(title)
	if err := resultError(result); err != nil {
		return nil, err
	}
	return &nativeNode{ptr: pointer}, nil
}

func (value *nativeImage) free() {
	C.fx_image_free(value.ptr)
	value.ptr = nil
}

func newNativeVideo(path string) (*nativeVideo, error) {
	data := []byte(path)
	var pointer *C.FxVideo
	result := C.fx_video_from_path(bytePointer(data), C.size_t(len(data)), &pointer)
	runtime.KeepAlive(data)
	if err := resultError(result); err != nil {
		return nil, err
	}
	return &nativeVideo{ptr: pointer}, nil
}

func (value *nativeVideo) node(style Style, fit ImageFit) (*nativeNode, error) {
	nativeStyle, title := nativeStyle(style)
	var pointer *C.FxNode
	result := C.fx_video_node(value.ptr, nativeStyle, C.uint32_t(fit), &pointer)
	runtime.KeepAlive(title)
	if err := resultError(result); err != nil {
		return nil, err
	}
	return &nativeNode{ptr: pointer}, nil
}

func (value *nativeVideo) free() {
	C.fx_video_free(value.ptr)
	value.ptr = nil
}

func (value *nativeVideo) play() error {
	return resultError(C.fx_video_play(value.ptr))
}

func (value *nativeVideo) pause() error {
	return resultError(C.fx_video_pause(value.ptr))
}

func (value *nativeVideo) togglePause() error {
	return resultError(C.fx_video_toggle_pause(value.ptr))
}

func (value *nativeVideo) seekForward() error {
	return resultError(C.fx_video_seek_forward(value.ptr))
}

func (value *nativeVideo) seekBackward() error {
	return resultError(C.fx_video_seek_backward(value.ptr))
}

func (value *nativeVideo) setVolume(volume uint8) error {
	return resultError(C.fx_video_set_volume(value.ptr, C.uint8_t(volume)))
}

func (value *nativeVideo) seekTo(position float64) error {
	return resultError(C.fx_video_seek_to(value.ptr, C.double(position)))
}

func (value *nativeVideo) state() (VideoState, error) {
	var paused C.uint8_t
	var volume C.uint8_t
	var position C.double
	var duration C.double
	if err := resultError(C.fx_video_state(value.ptr, &paused, &volume, &position, &duration)); err != nil {
		return VideoState{}, err
	}
	return VideoState{
		Paused:   paused != 0,
		Volume:   uint8(volume),
		Position: float64(position),
		Duration: float64(duration),
	}, nil
}

func nativeStyle(value Style) (C.FxStyle, []byte) {
	result := C.FxStyle{
		x:               C.uint16_t(value.X),
		y:               C.uint16_t(value.Y),
		padding_kind:    C.uint32_t(value.Padding.Kind),
		padding:         C.uint8_t(value.Padding.Value),
		gap:             C.uint16_t(value.Gap),
		position:        C.uint32_t(value.Position),
		z:               C.int16_t(value.Z),
		align_items:     C.uint32_t(nativeAlign(value.AlignItems)),
		flex_direction:  C.uint32_t(value.Direction),
		justify_content: C.uint32_t(value.Justify),
	}
	if value.Width != nil {
		result.has_width = 1
		result.width = C.uint16_t(*value.Width)
	}
	if value.Height != nil {
		result.has_height = 1
		result.height = C.uint16_t(*value.Height)
	}
	if value.Background != nil {
		result.has_background = 1
		result.background_color = C.uint32_t(*value.Background)
	}
	if value.AlignSelf != nil {
		result.has_align_self = 1
		result.align_self = C.uint32_t(nativeAlign(*value.AlignSelf))
	}
	var title []byte
	if value.Border != nil {
		result.has_border = 1
		result.border_color = C.uint32_t(value.Border.Color)
		result.title_color = result.border_color
		if value.Border.TitleColor != nil {
			result.title_color = C.uint32_t(*value.Border.TitleColor)
		}
		result.title_alignment = C.uint32_t(value.Border.TitleAlignment)
		title = []byte(value.Border.Title)
		result.title = bytePointer(title)
		result.title_len = C.size_t(len(title))
		if value.Border.Media != nil && value.Border.Media.native != nil {
			result.media_video = value.Border.Media.native.ptr
		}
	}
	return result, title
}

func nativeAlign(value Align) uint32 {
	switch value {
	case AlignStart:
		return 0
	case AlignEnd:
		return 1
	case AlignCenter:
		return 2
	default:
		return 3
	}
}
