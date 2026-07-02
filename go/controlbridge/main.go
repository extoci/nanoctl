package main

import (
	"bufio"
	"encoding/json"
	"fmt"
	"os"
	"strings"

	"github.com/BurntSushi/xgb"
	"github.com/BurntSushi/xgb/xproto"
	"github.com/BurntSushi/xgb/xtest"
)

type controlEvent struct {
	Kind   string  `json:"kind"`
	Action string  `json:"action"`
	X      float64 `json:"x"`
	Y      float64 `json:"y"`
	Button int     `json:"button"`
	Key    string  `json:"key"`
}

type x11Bridge struct {
	conn *xgb.Conn
	root xproto.Window
}

func main() {
	bridge, err := newX11Bridge()
	if err != nil {
		fmt.Fprintf(os.Stderr, "x11 unavailable: %v\n", err)
		os.Exit(1)
	}
	defer bridge.close()

	if len(os.Args) > 1 && os.Args[1] == "--probe" {
		fmt.Println("ready")
		return
	}

	scanner := bufio.NewScanner(os.Stdin)
	for scanner.Scan() {
		line := strings.TrimSpace(scanner.Text())
		if line == "" {
			continue
		}

		var event controlEvent
		if err := json.Unmarshal([]byte(line), &event); err != nil {
			fmt.Fprintf(os.Stderr, "invalid payload: %v\n", err)
			continue
		}

		if err := bridge.dispatch(event); err != nil {
			fmt.Fprintf(os.Stderr, "dispatch failed: %v\n", err)
		}
	}

	if err := scanner.Err(); err != nil {
		fmt.Fprintf(os.Stderr, "stdin error: %v\n", err)
		os.Exit(1)
	}
}

func newX11Bridge() (*x11Bridge, error) {
	conn, err := xgb.NewConn()
	if err != nil {
		return nil, err
	}

	if err := xtest.Init(conn); err != nil {
		conn.Close()
		return nil, err
	}

	setup := xproto.Setup(conn)
	screen := setup.DefaultScreen(conn)
	return &x11Bridge{
		conn: conn,
		root: screen.Root,
	}, nil
}

func (b *x11Bridge) close() {
	if b.conn != nil {
		b.conn.Close()
	}
}

func (b *x11Bridge) dispatch(event controlEvent) error {
	switch event.Kind {
	case "pointer":
		return b.dispatchPointer(event)
	case "key":
		return b.dispatchKey(event)
	default:
		return fmt.Errorf("unsupported kind %q", event.Kind)
	}
}

func (b *x11Bridge) dispatchPointer(event controlEvent) error {
	width, height, err := b.getDisplayGeometry()
	if err != nil {
		return err
	}

	x := int16(clamp(event.X) * float64(width-1))
	y := int16(clamp(event.Y) * float64(height-1))

	switch event.Action {
	case "move":
		return b.movePointer(x, y)
	case "down":
		if err := b.movePointer(x, y); err != nil {
			return err
		}
		return b.fakeButton(xproto.ButtonPress, mapButton(event.Button))
	case "up":
		if err := b.movePointer(x, y); err != nil {
			return err
		}
		return b.fakeButton(xproto.ButtonRelease, mapButton(event.Button))
	case "click":
		if err := b.movePointer(x, y); err != nil {
			return err
		}
		button := mapButton(event.Button)
		if err := b.fakeButton(xproto.ButtonPress, button); err != nil {
			return err
		}
		return b.fakeButton(xproto.ButtonRelease, button)
	default:
		return fmt.Errorf("unsupported pointer action %q", event.Action)
	}
}

func (b *x11Bridge) dispatchKey(event controlEvent) error {
	keysym, ok := mapKeysym(event.Key)
	if !ok {
		return nil
	}

	keycode, err := b.lookupKeycode(keysym)
	if err != nil {
		return err
	}

	switch event.Action {
	case "press", "down":
		if err := b.fakeKey(xproto.KeyPress, keycode); err != nil {
			return err
		}
		if event.Action == "press" {
			return b.fakeKey(xproto.KeyRelease, keycode)
		}
		return nil
	case "up":
		return b.fakeKey(xproto.KeyRelease, keycode)
	default:
		return fmt.Errorf("unsupported key action %q", event.Action)
	}
}

func (b *x11Bridge) movePointer(x int16, y int16) error {
	if err := xproto.WarpPointerChecked(b.conn, 0, b.root, 0, 0, 0, 0, x, y).Check(); err != nil {
		return err
	}
	b.conn.Sync()
	return nil
}

func (b *x11Bridge) fakeButton(eventType byte, button byte) error {
	if err := xtest.FakeInputChecked(b.conn, eventType, button, 0, b.root, 0, 0, 0).Check(); err != nil {
		return err
	}
	b.conn.Sync()
	return nil
}

func (b *x11Bridge) fakeKey(eventType byte, keycode xproto.Keycode) error {
	if err := xtest.FakeInputChecked(b.conn, eventType, byte(keycode), 0, b.root, 0, 0, 0).Check(); err != nil {
		return err
	}
	b.conn.Sync()
	return nil
}

func (b *x11Bridge) getDisplayGeometry() (int, int, error) {
	geometry, err := xproto.GetGeometry(b.conn, xproto.Drawable(b.root)).Reply()
	if err != nil {
		return 0, 0, err
	}
	return int(geometry.Width), int(geometry.Height), nil
}

func (b *x11Bridge) lookupKeycode(keysym xproto.Keysym) (xproto.Keycode, error) {
	setup := xproto.Setup(b.conn)
	minKeycode := setup.MinKeycode
	maxKeycode := setup.MaxKeycode
	count := byte(int(maxKeycode-minKeycode) + 1)

	reply, err := xproto.GetKeyboardMapping(b.conn, minKeycode, count).Reply()
	if err != nil {
		return 0, err
	}

	for idx := 0; idx < int(count); idx++ {
		base := idx * int(reply.KeysymsPerKeycode)
		for offset := 0; offset < int(reply.KeysymsPerKeycode); offset++ {
			if reply.Keysyms[base+offset] == keysym {
				return xproto.Keycode(byte(int(minKeycode) + idx)), nil
			}
		}
	}

	return 0, fmt.Errorf("no keycode found for keysym %#x", uint32(keysym))
}

func clamp(value float64) float64 {
	if value < 0 {
		return 0
	}
	if value > 1 {
		return 1
	}
	return value
}

func mapButton(button int) byte {
	switch button {
	case 1:
		return 2
	case 2:
		return 3
	default:
		return 1
	}
}

func mapKeysym(key string) (xproto.Keysym, bool) {
	switch key {
	case " ":
		return 0x0020, true
	case "ArrowUp":
		return 0xff52, true
	case "ArrowDown":
		return 0xff54, true
	case "ArrowLeft":
		return 0xff51, true
	case "ArrowRight":
		return 0xff53, true
	case "Escape":
		return 0xff1b, true
	case "Enter":
		return 0xff0d, true
	case "Backspace":
		return 0xff08, true
	case "Tab":
		return 0xff09, true
	case "Delete":
		return 0xffff, true
	case "Shift":
		return 0xffe1, true
	case "Control":
		return 0xffe3, true
	case "Alt":
		return 0xffe9, true
	case "Meta":
		return 0xffeb, true
	default:
		if len(key) == 1 {
			return xproto.Keysym(key[0]), true
		}
		return 0, false
	}
}
