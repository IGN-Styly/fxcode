package main

import (
	"log"
	"math"

	"github.com/IGN-Styly/fxcode/go/fxcode"
)

func main() {
	runtime, err := fxcode.NewRuntime(nil)
	if err != nil {
		log.Fatal(err)
	}
	defer runtime.Close()

	video, err := fxcode.NewVideo("docs/cars.mp4")
	if err != nil {
		log.Fatal(err)
	}
	defer video.Close()

	size, err := runtime.Size()
	if err != nil {
		log.Fatal(err)
	}
	app := fxcode.NewApp(view(video, size))

	err = runtime.Run(app, func(app *fxcode.App, event fxcode.Event) fxcode.ControlFlow {
		if event.Kind == fxcode.EventKey &&
			(event.Key == fxcode.KeyEsc || event.Key == fxcode.KeyChar && event.Rune == 'q') {
			return fxcode.Exit
		}
		if event.Kind == fxcode.EventResize {
			app.Root = view(video, event.Size)
			return fxcode.Render
		}
		if event.Kind == fxcode.EventInit {
			return fxcode.Render
		}
		return fxcode.Continue
	})
	if err != nil {
		log.Fatal(err)
	}
}

func view(video *fxcode.Video, terminal fxcode.TerminalSize) *fxcode.Container {
	videoWidth, videoHeight := videoSize(terminal)
	video.Fit = fxcode.ImageContain
	video.Style = fxcode.Style{Z: 11}

	modal := &fxcode.Container{
		Style: fxcode.Style{
			Width:      fxcode.Cells(videoWidth + 2),
			Height:     fxcode.Cells(videoHeight + 2),
			Border:     &fxcode.Border{Color: fxcode.White, Title: "Cars", Media: video},
			Background: fxcode.ColorValue(fxcode.Black),
			Z:          10,
		},
		Children: []fxcode.Node{video},
	}

	return &fxcode.Container{
		Style: fxcode.Style{
			Border:     &fxcode.Border{Color: fxcode.White, Title: "fxcode"},
			Background: fxcode.ColorValue(fxcode.Black),
			AlignItems: fxcode.AlignCenter,
			Justify:    fxcode.JustifyCenter,
		},
		Children: []fxcode.Node{modal},
	}
}

func videoSize(terminal fxcode.TerminalSize) (uint16, uint16) {
	const videoWidth = 1920.0
	const videoHeight = 800.0

	maxWidth := min(saturatingSub(terminal.Cols, 4), 64)
	maxHeight := saturatingSub(terminal.Rows, 4)
	if maxWidth == 0 || maxHeight == 0 {
		return 0, 0
	}

	cellWidth := 8.0
	if terminal.PixelWidth > 0 && terminal.Cols > 0 {
		cellWidth = float64(terminal.PixelWidth) / float64(terminal.Cols)
	}
	cellHeight := 16.0
	if terminal.PixelHeight > 0 && terminal.Rows > 0 {
		cellHeight = float64(terminal.PixelHeight) / float64(terminal.Rows)
	}
	scale := math.Min(
		float64(maxWidth)*cellWidth/videoWidth,
		float64(maxHeight)*cellHeight/videoHeight,
	)

	width := uint16(math.Round(videoWidth * scale / cellWidth))
	height := uint16(math.Round(videoHeight * scale / cellHeight))
	return min(max(width, 1), maxWidth), min(max(height, 1), maxHeight)
}

func saturatingSub(value, amount uint16) uint16 {
	if value < amount {
		return 0
	}
	return value - amount
}
