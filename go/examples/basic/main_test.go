package main

import (
	"math"
	"testing"

	"github.com/IGN-Styly/fxcode/go/fxcode"
)

func TestCarsPreviewKeepsItsPixelAspectRatio(t *testing.T) {
	width, height := videoSize(fxcode.TerminalSize{
		Cols:        100,
		Rows:        50,
		PixelWidth:  800,
		PixelHeight: 800,
	})
	ratio := float64(width) * 8 / (float64(height) * 16)
	if math.Abs(ratio-2.4) >= 0.1 {
		t.Fatalf("video ratio is %f, want about 2.4", ratio)
	}
}
