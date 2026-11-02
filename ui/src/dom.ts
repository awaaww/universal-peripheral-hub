//! Minimal DOM helpers. No framework: the panel renders descriptors, so the
//! markup is generated in one place and stays easy to audit.

export type AttrValue =
  | string
  | number
  | boolean
  | undefined
  | null
  | ((event: Event) => void);

type Attrs = Record<string, AttrValue>;

export function el<K extends keyof HTMLElementTagNameMap>(
  tag: K,
  attrs: Attrs = {},
  children: (Node | string)[] = [],
): HTMLElementTagNameMap[K] {
  const node = document.createElement(tag);
  for (const [key, value] of Object.entries(attrs)) {
    if (value === undefined || value === null || value === false) continue;
    if (typeof value === "function") {
      // `onclick`, `onchange`, `oninput` — the DOM property names we use.
      (node as unknown as Record<string, unknown>)[key] = value;
    } else if (key === "class") node.className = String(value);
    else if (key === "text") node.textContent = String(value);
    else if (key === "html") node.innerHTML = String(value);
    else node.setAttribute(key, value === true ? "" : String(value));
  }
  for (const child of children) {
    node.append(typeof child === "string" ? document.createTextNode(child) : child);
  }
  return node;
}

export function clear(node: HTMLElement) {
  node.replaceChildren();
}
