import { el } from "./dom";
import type { CapabilityDescriptor, CapabilityValue, DeviceDescriptor } from "./protocol";
import type { Actions, UiState } from "./render";

/** One keycode the picker offers. `code` is the big-endian VIA keycode. */
interface Keycode {
  code: number;
  label: string;
}

const MODIFIER: Keycode[] = [
  { code: 0x00e0, label: "LCtrl" },
  { code: 0x00e1, label: "LShift" },
  { code: 0x00e2, label: "LAlt" },
  { code: 0x00e3, label: "LGui" },
  { code: 0x00e4, label: "RCtrl" },
  { code: 0x00e5, label: "RShift" },
  { code: 0x00e6, label: "RAlt" },
  { code: 0x00e7, label: "RGui" },
];

const FUNCTION: Keycode[] = Array.from({ length: 24 }, (_, index) => ({
  code: index < 12 ? 0x003a + index : 0x0068 + (index - 12),
  label: `F${index + 1}`,
}));

const LETTERS: Keycode[] = Array.from({ length: 26 }, (_, index) => ({
  code: 0x0004 + index,
  label: String.fromCharCode(0x41 + index),
}));

const DIGITS: Keycode[] = Array.from({ length: 10 }, (_, index) => ({
  code: index === 9 ? 0x0027 : 0x001e + index,
  label: index === 9 ? "0" : String(index + 1),
}));

const PUNCTUATION: Keycode[] = [
  { code: 0x002d, label: "-" },
  { code: 0x002e, label: "=" },
  { code: 0x002f, label: "[" },
  { code: 0x0030, label: "]" },
  { code: 0x0031, label: "\\" },
  { code: 0x0033, label: ";" },
  { code: 0x0034, label: "'" },
  { code: 0x0035, label: "`" },
  { code: 0x0036, label: "," },
  { code: 0x0037, label: "." },
  { code: 0x0038, label: "/" },
];

const NAMED: Keycode[] = [
  { code: 0x0001, label: "▽ Transparent" },
  { code: 0x0028, label: "Enter" },
  { code: 0x0029, label: "Esc" },
  { code: 0x002a, label: "Backspace" },
  { code: 0x002b, label: "Tab" },
  { code: 0x002c, label: "Space" },
  { code: 0x0039, label: "Caps Lock" },
  { code: 0x0046, label: "Print Screen" },
  { code: 0x0047, label: "Scroll Lock" },
  { code: 0x0048, label: "Pause" },
  { code: 0x0049, label: "Insert" },
  { code: 0x004a, label: "Home" },
  { code: 0x004b, label: "Page Up" },
  { code: 0x004c, label: "Delete" },
  { code: 0x004d, label: "End" },
  { code: 0x004e, label: "Page Down" },
  { code: 0x004f, label: "→" },
  { code: 0x0050, label: "←" },
  { code: 0x0051, label: "↓" },
  { code: 0x0052, label: "↑" },
  { code: 0x0053, label: "Num Lock" },
  { code: 0x007a, label: "Mute" },
  { code: 0x007b, label: "Volume −" },
  { code: 0x007c, label: "Volume +" },
];

/** Everything the picker offers, in menu order. */
export const KEYCODES: Keycode[] = [
  ...NAMED,
  { code: 0x0000, label: "∅ None (KC_NO)" },
  ...MODIFIER,
  ...LETTERS,
  ...DIGITS,
  ...PUNCTUATION,
  ...FUNCTION,
];

const BY_CODE = new Map(KEYCODES.map((keycode) => [keycode.code, keycode.label]));

/** A readable label for a keycode, falling back to its hex value. */
export function labelFor(code: number): string {
  return BY_CODE.get(code) ?? `0x${code.toString(16).padStart(4, "0")}`;
}

/**
 * A grid-sized label: one glyph or word per 16-column button.
 *
 * The full label is still in the button's tooltip, and the picker shows it in
 * full; sixteen columns on a laptop window cannot fit "Page Down".
 */
export function shortLabelFor(code: number): string {
  if (code === 0) return "∅";
  if (code === 1) return "▽";
  const label = BY_CODE.get(code);
  if (!label) return code.toString(16).toUpperCase().padStart(4, "0");
  switch (label) {
    case "Print Screen":
      return "PrtSc";
    case "Scroll Lock":
      return "ScrLk";
    case "Page Up":
      return "PgUp";
    case "Page Down":
      return "PgDn";
    case "Caps Lock":
      return "Caps";
    case "Backspace":
      return "Bksp";
    case "Delete":
      return "Del";
    case "Insert":
      return "Ins";
    case "Volume −":
      return "Vol−";
    case "Volume +":
      return "Vol+";
    case "Num Lock":
      return "Num";
    default:
      return label.split(" ")[0] ?? label;
  }
}

interface EditorState {
  layer: number;
  selected: number | null;
}

const editors = new Map<string, EditorState>();

function editorState(deviceId: string): EditorState {
  let state = editors.get(deviceId);
  if (!state) {
    state = { layer: 1, selected: null };
    editors.set(deviceId, state);
  }
  return state;
}

function layersOf(state: UiState, deviceId: string): number {
  const value = state.values.get(`${deviceId}/keyboard.layers`);
  return value?.type === "int" && Number.isInteger(value.value) && value.value >= 1
    ? value.value
    : 4;
}

function keysPerLayer(image: number[], layers: number): number {
  const total = image.length / 2 / layers;
  return Number.isInteger(total) && total >= 1 ? total : 96;
}

function codeAt(image: number[], index: number): number {
  return ((image[index * 2] ?? 0) << 8) | (image[index * 2 + 1] ?? 0);
}

function withCode(image: number[], index: number, code: number): number[] {
  const next = image.slice();
  next[index * 2] = (code >> 8) & 0xff;
  next[index * 2 + 1] = code & 0xff;
  return next;
}

/**
 * The keymap editor: a layer tab strip and one button per key, plus a picker
 * for the selected key. The whole image is written back through the normal
 * capability path, so the read-back and error handling stay in one place.
 */
export function keymapEditor(
  device: DeviceDescriptor,
  capability: CapabilityDescriptor,
  value: CapabilityValue,
  state: UiState,
  actions: Actions,
): HTMLElement {
  const editor = editorState(device.id);
  const image = value.type === "bytes" ? value.value : [];
  const layers = layersOf(state, device.id);
  const keys = keysPerLayer(image, layers);
  const layer = Math.min(Math.max(editor.layer, 1), layers);
  editor.layer = layer;

  const tabs = el("div", { class: "keymap-layers" });
  for (let index = 1; index <= layers; index += 1) {
    tabs.append(
      el("button", {
        type: "button",
        class: index === layer ? "layer-tab active" : "layer-tab",
        text: `Layer ${index}`,
        onclick: () => {
          editor.layer = index;
          editor.selected = null;
          actions.redraw();
        },
      }),
    );
  }

  const grid = el("div", { class: "keymap-grid" });
  const base = (layer - 1) * keys;
  for (let index = 0; index < keys; index += 1) {
    const code = codeAt(image, base + index);
    grid.append(
      el("button", {
        type: "button",
        class: index === editor.selected ? "key active" : "key",
        text: shortLabelFor(code),
        title: `Key ${index + 1} · ${labelFor(code)}`,
        onclick: () => {
          editor.selected = editor.selected === index ? null : index;
          actions.redraw();
        },
      }),
    );
  }

  const children: HTMLElement[] = [
    el("div", { class: "keymap-head" }, [
      el("span", { class: "muted", text: `${layers} layers · ${keys} keys per layer` }),
      tabs,
    ]),
    grid,
  ];

  if (editor.selected !== null && editor.selected < keys) {
    const index = editor.selected;
    const current = codeAt(image, base + index);
    const select = el("select", { class: "keycode-picker" });
    for (const keycode of KEYCODES) {
      const option = el("option", { value: String(keycode.code), text: keycode.label });
      if (keycode.code === current) option.setAttribute("selected", "selected");
      select.append(option);
    }
    children.push(
      el("div", { class: "keymap-picker" }, [
        el("span", { class: "muted", text: `Key ${index + 1}, layer ${layer}:` }),
        select,
        el("button", {
          type: "button",
          class: "primary",
          text: "Apply",
          onclick: () => {
            const code = Number((select as HTMLSelectElement).value);
            const next = withCode(image, base + index, code);
            editor.selected = null;
            actions.write(device, capability, { type: "bytes", value: next });
          },
        }),
        el("button", {
          type: "button",
          text: "Cancel",
          onclick: () => {
            editor.selected = null;
            actions.redraw();
          },
        }),
      ]),
    );
  }

  children.push(
    el("p", {
      class: "muted keymap-note",
      text: "Writes the whole keymap image: the adapter sends only the changed keycodes, reads them back, and restores the original image if the board disagrees. The pre-edit image is kept in %APPDATA%\\UniversalPeripheralHub\\keymap-backups.",
    }),
  );

  return el("div", { class: "keymap" }, children);
}
