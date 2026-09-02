package fxcode

import (
	"errors"
	"sync"
)

type Image struct {
	mu     sync.Mutex
	native *nativeImage
	Style  Style
	Fit    ImageFit
}

func NewImage(path string) (*Image, error) {
	native, err := newNativeImageFromPath(path)
	if err != nil {
		return nil, err
	}
	return &Image{native: native, Fit: ImageFill}, nil
}

func NewRGBAImage(width, height uint32, pixels []byte) (*Image, error) {
	native, err := newNativeImageFromRGBA(width, height, pixels)
	if err != nil {
		return nil, err
	}
	return &Image{native: native, Fit: ImageFill}, nil
}

func NewPNGImage(data []byte) (*Image, error) {
	native, err := newNativeImageFromPNG(data)
	if err != nil {
		return nil, err
	}
	return &Image{native: native, Fit: ImageFill}, nil
}

func (value *Image) Close() error {
	value.mu.Lock()
	defer value.mu.Unlock()
	if value.native != nil {
		value.native.free()
		value.native = nil
	}
	return nil
}

func (value *Image) build() (*nativeNode, error) {
	value.mu.Lock()
	defer value.mu.Unlock()
	if value.native == nil {
		return nil, errors.New("image is closed")
	}
	return value.native.node(value.Style, value.Fit)
}

type Video struct {
	mu     sync.Mutex
	native *nativeVideo
	Style  Style
	Fit    ImageFit
}

func NewVideo(path string) (*Video, error) {
	native, err := newNativeVideo(path)
	if err != nil {
		return nil, err
	}
	return &Video{native: native, Fit: ImageContain}, nil
}

func (value *Video) Close() error {
	value.mu.Lock()
	defer value.mu.Unlock()
	if value.native != nil {
		value.native.free()
		value.native = nil
	}
	return nil
}

func (value *Video) build() (*nativeNode, error) {
	value.mu.Lock()
	defer value.mu.Unlock()
	if value.native == nil {
		return nil, errors.New("video is closed")
	}
	return value.native.node(value.Style, value.Fit)
}

func (value *Video) Play() error {
	return value.action((*nativeVideo).play)
}

func (value *Video) Pause() error {
	return value.action((*nativeVideo).pause)
}

func (value *Video) TogglePause() error {
	return value.action((*nativeVideo).togglePause)
}

func (value *Video) SeekForward() error {
	return value.action((*nativeVideo).seekForward)
}

func (value *Video) SeekBackward() error {
	return value.action((*nativeVideo).seekBackward)
}

func (value *Video) action(action func(*nativeVideo) error) error {
	value.mu.Lock()
	defer value.mu.Unlock()
	if value.native == nil {
		return errors.New("video is closed")
	}
	return action(value.native)
}

func (value *Video) SetVolume(volume uint8) error {
	value.mu.Lock()
	defer value.mu.Unlock()
	if value.native == nil {
		return errors.New("video is closed")
	}
	return value.native.setVolume(volume)
}

func (value *Video) SeekTo(position float64) error {
	value.mu.Lock()
	defer value.mu.Unlock()
	if value.native == nil {
		return errors.New("video is closed")
	}
	return value.native.seekTo(position)
}

func (value *Video) State() (VideoState, error) {
	value.mu.Lock()
	defer value.mu.Unlock()
	if value.native == nil {
		return VideoState{}, errors.New("video is closed")
	}
	return value.native.state()
}
