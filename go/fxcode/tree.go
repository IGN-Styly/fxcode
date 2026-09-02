package fxcode

import "errors"

type Node interface {
	build() (*nativeNode, error)
}

type App struct {
	Root Node
}

func NewApp(root Node) *App {
	return &App{Root: root}
}

type Container struct {
	Style    Style
	Children []Node
}

func NewContainer(children ...Node) *Container {
	return &Container{Children: children}
}

func (value *Container) build() (*nativeNode, error) {
	if value == nil {
		return nil, errors.New("container is nil")
	}
	parent, err := newNativeContainer(value.Style)
	if err != nil {
		return nil, err
	}
	for _, child := range value.Children {
		if child == nil {
			parent.free()
			return nil, errors.New("container child is nil")
		}
		nativeChild, err := child.build()
		if err != nil {
			parent.free()
			return nil, err
		}
		err = parent.add(nativeChild)
		nativeChild.free()
		if err != nil {
			parent.free()
			return nil, err
		}
	}
	return parent, nil
}
