package main

import (
	"log"

	"github.com/IGN-Styly/fxcode/go/fxcode"
)

func main() {
	runtime, err := fxcode.NewRuntime(nil)
	if err != nil {
		log.Fatal(err)
	}
	defer runtime.Close()

	app := fxcode.NewApp(&fxcode.Container{
		Style: fxcode.Style{
			Border:     &fxcode.Border{Color: fxcode.White, Title: "fxcode-go"},
			Background: fxcode.ColorValue(fxcode.Black),
		},
	})

	err = runtime.Run(app, func(_ *fxcode.App, event fxcode.Event) fxcode.ControlFlow {
		if event.Kind == fxcode.EventKey &&
			(event.Key == fxcode.KeyEsc || event.Key == fxcode.KeyChar && event.Rune == 'q') {
			return fxcode.Exit
		}
		if event.Kind == fxcode.EventInit || event.Kind == fxcode.EventResize {
			return fxcode.Render
		}
		return fxcode.Continue
	})
	if err != nil {
		log.Fatal(err)
	}
}
