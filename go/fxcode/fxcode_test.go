package fxcode

import "testing"

func TestRGBAImageBuildAndClose(t *testing.T) {
	image, err := NewRGBAImage(1, 1, []byte{255, 0, 0, 255})
	if err != nil {
		t.Fatal(err)
	}
	root := NewContainer(image)
	node, err := root.build()
	if err != nil {
		t.Fatal(err)
	}
	node.free()

	if err := image.Close(); err != nil {
		t.Fatal(err)
	}
	if err := image.Close(); err != nil {
		t.Fatal(err)
	}
	if _, err := image.build(); err == nil {
		t.Fatal("closed image built without an error")
	}
}

func TestRGBAImageChecksPixelLength(t *testing.T) {
	if _, err := NewRGBAImage(2, 1, []byte{0, 0, 0, 0}); err == nil {
		t.Fatal("invalid pixel data did not return an error")
	}
}

func TestContainerRejectsNilChild(t *testing.T) {
	root := NewContainer(nil)
	if _, err := root.build(); err == nil {
		t.Fatal("nil child did not return an error")
	}
}

func TestContainerCopiesBorderTitle(t *testing.T) {
	root := &Container{Style: Style{Border: &Border{Color: White, Title: "hello"}}}
	node, err := root.build()
	if err != nil {
		t.Fatal(err)
	}
	node.free()
}

func TestPNGImageRejectsInvalidData(t *testing.T) {
	if _, err := NewPNGImage([]byte("not a PNG")); err == nil {
		t.Fatal("invalid PNG did not return an error")
	}
}

func TestColorPacking(t *testing.T) {
	if got := RGBA(1, 2, 3, 4); got != Color(0x01020304) {
		t.Fatalf("RGBA returned %#x", got)
	}
}
