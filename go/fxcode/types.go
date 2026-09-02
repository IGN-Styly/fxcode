package fxcode

type Color uint32

const (
	Black   Color = 0x000000ff
	White   Color = 0xffffffff
	Red     Color = 0xff0000ff
	Green   Color = 0x00ff00ff
	Blue    Color = 0x0000ffff
	Yellow  Color = 0xffff00ff
	Cyan    Color = 0x00ffffff
	Magenta Color = 0xff00ffff
	Gray    Color = 0x808080ff
	Orange  Color = 0xffa500ff
	Purple  Color = 0x800080ff
	Pink    Color = 0xffc0cbff
)

func RGBA(red, green, blue, alpha uint8) Color {
	return Color(uint32(red)<<24 | uint32(green)<<16 | uint32(blue)<<8 | uint32(alpha))
}

type PaddingKind uint32

const (
	PaddingAll PaddingKind = iota
	PaddingTop
	PaddingBottom
	PaddingRight
	PaddingLeft
	PaddingHorizontal
	PaddingVertical
)

type Padding struct {
	Kind  PaddingKind
	Value uint8
}

type Position uint32

const (
	Relative Position = iota
	Absolute
)

type Align uint32

const (
	AlignStretch Align = iota
	AlignStart
	AlignEnd
	AlignCenter
)

type FlexDirection uint32

const (
	Column FlexDirection = iota
	ColumnReverse
	Row
	RowReverse
)

type JustifyContent uint32

const (
	JustifyStart JustifyContent = iota
	JustifyEnd
	JustifyCenter
)

type TitleAlignment uint32

const (
	TitleLeft TitleAlignment = iota
	TitleCenter
	TitleRight
)

type ImageFit uint32

const (
	ImageFill ImageFit = iota
	ImageContain
	ImageCover
	ImageOriginal
)

type KittyMode uint32

const (
	KittyAuto KittyMode = iota
	KittyEnabled
	KittyDisabled
)

type Border struct {
	Color          Color
	Title          string
	TitleColor     *Color
	TitleAlignment TitleAlignment
	Media          *Video
}

type Style struct {
	Width      *uint16
	Height     *uint16
	X          uint16
	Y          uint16
	Padding    Padding
	Border     *Border
	Background *Color
	Gap        uint16
	Position   Position
	Z          int16
	AlignItems Align
	AlignSelf  *Align
	Direction  FlexDirection
	Justify    JustifyContent
}

func Cells(value uint16) *uint16 { return &value }

func ColorValue(value Color) *Color { return &value }

func AlignValue(value Align) *Align { return &value }

type TerminalSize struct {
	Cols        uint16
	Rows        uint16
	PixelWidth  uint16
	PixelHeight uint16
}

type EventKind uint32

const (
	EventInit EventKind = iota
	EventKey
	EventMouse
	EventResize
	eventEnd
)

type Key uint32

const (
	KeyUnknown Key = iota
	KeyBackspace
	KeyLeft
	KeyShiftLeft
	KeyAltLeft
	KeyCtrlLeft
	KeyRight
	KeyShiftRight
	KeyAltRight
	KeyCtrlRight
	KeyUp
	KeyShiftUp
	KeyAltUp
	KeyCtrlUp
	KeyDown
	KeyShiftDown
	KeyAltDown
	KeyCtrlDown
	KeyHome
	KeyCtrlHome
	KeyEnd
	KeyCtrlEnd
	KeyPageUp
	KeyPageDown
	KeyBackTab
	KeyDelete
	KeyInsert
	KeyFunction
	KeyChar
	KeyAltChar
	KeyCtrlChar
	KeyNull
	KeyEsc
)

type MouseKind uint32

const (
	MouseUnknown MouseKind = iota
	MousePress
	MouseRelease
	MouseHold
)

type MouseButton uint32

const (
	MouseNoButton MouseButton = iota
	MouseLeft
	MouseRight
	MouseMiddle
	MouseWheelUp
	MouseWheelDown
	MouseWheelLeft
	MouseWheelRight
)

type Event struct {
	Kind        EventKind
	Key         Key
	Rune        rune
	Function    uint8
	MouseKind   MouseKind
	MouseButton MouseButton
	X           uint16
	Y           uint16
	Size        TerminalSize
}

type ControlFlow uint8

const (
	Continue ControlFlow = iota
	Render
	Exit
)

type Config struct {
	Kitty       KittyMode
	EventWaitMS uint32
}

func DefaultConfig() Config {
	return Config{Kitty: KittyAuto, EventWaitMS: 16}
}

type VideoState struct {
	Paused   bool
	Volume   uint8
	Position float64
	Duration float64
}
