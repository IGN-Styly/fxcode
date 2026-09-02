package fxcode

import (
	"errors"
	"sync"
)

type Runtime struct {
	mu     sync.Mutex
	native *nativeRuntime
}

func NewRuntime(config *Config) (*Runtime, error) {
	value := DefaultConfig()
	if config != nil {
		value = *config
	}
	native, err := newNativeRuntime(value)
	if err != nil {
		return nil, err
	}
	return &Runtime{native: native}, nil
}

func (value *Runtime) Close() error {
	value.mu.Lock()
	defer value.mu.Unlock()
	if value.native != nil {
		value.native.free()
		value.native = nil
	}
	return nil
}

func (value *Runtime) Size() (TerminalSize, error) {
	value.mu.Lock()
	defer value.mu.Unlock()
	if value.native == nil {
		return TerminalSize{}, errors.New("runtime is closed")
	}
	return value.native.size()
}

func (value *Runtime) Draw(app *App) error {
	if app == nil || app.Root == nil {
		return errors.New("app root is nil")
	}
	root, err := app.Root.build()
	if err != nil {
		return err
	}
	defer root.free()

	value.mu.Lock()
	defer value.mu.Unlock()
	if value.native == nil {
		return errors.New("runtime is closed")
	}
	return value.native.draw(root)
}

func (value *Runtime) nextEvent() (Event, error) {
	value.mu.Lock()
	defer value.mu.Unlock()
	if value.native == nil {
		return Event{}, errors.New("runtime is closed")
	}
	return value.native.nextEvent()
}

func (value *Runtime) Run(app *App, update func(*App, Event) ControlFlow) error {
	if app == nil {
		return errors.New("app is nil")
	}
	if update == nil {
		return errors.New("update function is nil")
	}
	if update(app, Event{Kind: EventInit}) == Exit {
		return nil
	}
	if err := value.Draw(app); err != nil {
		return err
	}

	for {
		event, err := value.nextEvent()
		if err != nil {
			return err
		}
		if event.Kind == eventEnd {
			return nil
		}
		flow := update(app, event)
		if flow == Exit {
			return nil
		}
		if flow == Render || event.Kind == EventResize {
			if err := value.Draw(app); err != nil {
				return err
			}
		}
	}
}
