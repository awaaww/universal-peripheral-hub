//! Tiny dependency-free i18n layer.
//!
//! Strings live in three flat dictionaries. `en` is the source of truth: every
//! other dictionary is typed as `Record<keyof typeof en, string>`, so a missing
//! or misspelled key is a compile error instead of a silent English leak.
//!
//! The panel has no reactive framework, so `setLanguage` only changes the
//! active dictionary; the caller repaints. Nothing here reloads the page.

import type { Language } from "./protocol";

const en = {
  "app.title": "Universal Peripheral Hub",
  "app.subtitle": "One process for displays, input, audio and more.",
  "app.connected": "hubd connected",
  "app.disconnected": "hubd not reachable",
  "app.rescan": "Rescan",
  "app.settings": "Settings",

  "offline.title": "Cannot reach the hub",
  "offline.body":
    "The resident core (hubd) is not answering. It is started automatically by the panel and by the tray icon.",
  "offline.retry": "Try again",

  "error.actionFailed": "That action failed",
  "error.unknown": "unknown error",

  "adapters.title": "Adapters",
  "adapters.enable": "Enable",
  "adapters.disable": "Disable",
  "adapters.deviceCount": "{count} device(s)",
  "adapters.disabled": "disabled",
  "adapters.builtIn": "built-in",
  "adapters.subprocess": "subprocess",
  "adapters.ignoredCount": "{count} ignored",
  "adapters.ignoredTitle": "{count} ignored device token(s) on {adapter}",
  "adapters.restore": "Restore",
  "adapters.restoreTitle": "Talk to {token} again.",

  "scenes.title": "Scenes",
  "scenes.empty": "No scenes yet. They live in %APPDATA%\\UniversalPeripheralHub\\scenes.json.",
  "scenes.current": "current",
  "scenes.selectedByRule": "Selected by the current rule.",
  "scenes.apply": "Apply",
  "scenes.applyTitle": "Apply “{name}” now.",
  "scenes.valuesCount": "{count} values",
  "scenes.noItems": "This scene has no items.",
  "scenes.baselineMissing": "Baseline not recorded yet (nothing will be written).",
  "scenes.appliedManually": "Applied manually: {name}",
  "scenes.autoMatch": "Auto: {exe} → {name}",
  "scenes.autoNoMatch": "Auto: no rule matches {exe}",
  "scenes.report": "Wrote {written} · unchanged {unchanged} · skipped {skipped}",
  "scenes.moreIssues": "{first} +{count} more",
  "scenes.allowSideEffects": "Allow changes that briefly blank the display (HDR, picture mode)",
  "scenes.applyAtStartup": "Apply at startup",

  "devices.loadingTitle": "Looking for devices…",
  "devices.emptyTitle": "No devices",
  "devices.loadingBody": "Asking every enabled adapter for its device list.",
  "devices.emptyBody": "All adapters are disabled, or no adapter reported a device.",
  "devices.ignore": "Ignore",
  "devices.ignoreTitle":
    "Stop talking to this monitor. Use this for a KVM or splitter that hangs the DDC bus.",

  "card.readOnly": "read-only",
  "card.renegotiate": "may renegotiate the signal",
  "card.writeOnly": "write-only",
  "card.reading": "reading…",
  "card.readBack": "read back from device",
  "card.rawBytes": "raw bytes",
  "card.bytes": "{count} bytes",
  "card.latency": "{ms} ms",

  "control.on": "On",
  "control.off": "Off",

  "status.saving": "Saving…",
  "status.synced": "Synced · read back {value}",
  "status.readbackFailed": "Applied, read-back failed: {error}",
  "status.deviceKept": "Device kept {value} instead",
  "status.retrying": "Retrying after {kind}…",

  "confirm.ignore":
    "Ignore “{name}”?\n\nStop talking to this monitor. Use this for a KVM or splitter that hangs the DDC bus.\n\nYou can restore it later from the adapters strip.",

  "footer.note": "Device cards are generated from adapter capability descriptors.",
  "footer.milestone": "M0 · mock adapter",

  "keymap.layer": "Layer {layer}",
  "keymap.layers": "{layers} layers · {keys} keys per layer",
  "keymap.key": "Key {index} · {label}",
  "keymap.keyAt": "Key {index}, layer {layer}:",
  "keymap.apply": "Apply",
  "keymap.cancel": "Cancel",
  "keymap.note":
    "Writes the whole keymap image: the adapter sends only the changed keycodes, reads them back, and restores the original image if the board disagrees. The pre-edit image is kept in %APPDATA%\\UniversalPeripheralHub\\keymap-backups.",

  "keycode.transparent": "Transparent",
  "keycode.none": "None (KC_NO)",
  "keycode.enter": "Enter",
  "keycode.escape": "Esc",
  "keycode.backspace": "Backspace",
  "keycode.tab": "Tab",
  "keycode.space": "Space",
  "keycode.capsLock": "Caps Lock",
  "keycode.printScreen": "Print Screen",
  "keycode.scrollLock": "Scroll Lock",
  "keycode.pause": "Pause",
  "keycode.insert": "Insert",
  "keycode.home": "Home",
  "keycode.pageUp": "Page Up",
  "keycode.delete": "Delete",
  "keycode.end": "End",
  "keycode.pageDown": "Page Down",
  "keycode.numLock": "Num Lock",
  "keycode.mute": "Mute",
  "keycode.volumeDown": "Volume −",
  "keycode.volumeUp": "Volume +",

  "battery.tooltip": "{label} · {percent}%",

  "settings.title": "Settings",
  "settings.language": "Language",
  "settings.closeBehaviour": "When closing the window",
  "settings.hideToTray": "Hide to tray",
  "settings.quitApp": "Quit the app",
  "settings.autoScan": "Scan for devices automatically",
  "settings.autoScanInterval": "Scan interval",
  "settings.interval": "{seconds} s",
  "settings.batteryAlert": "Alert on low battery",
  "settings.batteryThreshold": "Alert below this level (%)",
  "settings.save": "Save",
  "settings.cancel": "Cancel",
  "settings.loading": "Loading settings…",
  "settings.loadFailed": "Cannot load settings: {error}",
  "settings.saveFailed": "Cannot save settings: {error}",
};

type Key = keyof typeof en;

const zhCN: Record<Key, string> = {
  "app.title": "Universal Peripheral Hub",
  "app.subtitle": "一个进程，统管显示、输入、音频等外设。",
  "app.connected": "hubd 已连接",
  "app.disconnected": "hubd 无响应",
  "app.rescan": "重新扫描",
  "app.settings": "设置",

  "offline.title": "无法连接 hubd",
  "offline.body": "常驻核心（hubd）没有响应。它会由面板和托盘图标自动启动。",
  "offline.retry": "重试",

  "error.actionFailed": "操作失败",
  "error.unknown": "未知错误",

  "adapters.title": "适配器",
  "adapters.enable": "启用",
  "adapters.disable": "禁用",
  "adapters.deviceCount": "{count} 台设备",
  "adapters.disabled": "已禁用",
  "adapters.builtIn": "内置",
  "adapters.subprocess": "子进程",
  "adapters.ignoredCount": "已忽略 {count}",
  "adapters.ignoredTitle": "在 {adapter} 上有 {count} 个被忽略的设备令牌",
  "adapters.restore": "恢复",
  "adapters.restoreTitle": "重新与 {token} 通信。",

  "scenes.title": "场景",
  "scenes.empty": "还没有场景。它们保存在 %APPDATA%\\UniversalPeripheralHub\\scenes.json。",
  "scenes.current": "当前",
  "scenes.selectedByRule": "由当前规则选中。",
  "scenes.apply": "应用",
  "scenes.applyTitle": "立即应用“{name}”。",
  "scenes.valuesCount": "{count} 个值",
  "scenes.noItems": "此场景没有任何条目。",
  "scenes.baselineMissing": "尚未记录基线（不会写入任何内容）。",
  "scenes.appliedManually": "手动应用：{name}",
  "scenes.autoMatch": "自动：{exe} → {name}",
  "scenes.autoNoMatch": "自动：没有规则匹配 {exe}",
  "scenes.report": "写入 {written} · 未变 {unchanged} · 跳过 {skipped}",
  "scenes.moreIssues": "{first} 等另外 {count} 项",
  "scenes.allowSideEffects": "允许会短暂黑屏的更改（HDR、图像模式）",
  "scenes.applyAtStartup": "启动时应用",

  "devices.loadingTitle": "正在查找设备…",
  "devices.emptyTitle": "没有设备",
  "devices.loadingBody": "正在向每个已启用的适配器询问设备列表。",
  "devices.emptyBody": "所有适配器均已禁用，或没有适配器报告设备。",
  "devices.ignore": "忽略",
  "devices.ignoreTitle": "停止与此显示器通信。适用于会卡住 DDC 总线的 KVM 或分配器。",

  "card.readOnly": "只读",
  "card.renegotiate": "可能重新协商信号",
  "card.writeOnly": "只写",
  "card.reading": "读取中…",
  "card.readBack": "已从设备读回",
  "card.rawBytes": "原始字节",
  "card.bytes": "{count} 字节",
  "card.latency": "{ms} ms",

  "control.on": "开",
  "control.off": "关",

  "status.saving": "保存中…",
  "status.synced": "已同步 · 读回 {value}",
  "status.readbackFailed": "已应用，但读回失败：{error}",
  "status.deviceKept": "设备保留了 {value}",
  "status.retrying": "因 {kind} 重试…",

  "confirm.ignore":
    "忽略“{name}”吗？\n\n停止与该显示器通信。适用于会卡住 DDC 总线的 KVM 或分配器。\n\n之后可在适配器一栏中恢复。",

  "footer.note": "设备卡片由适配器能力描述生成。",
  "footer.milestone": "M0 · 模拟适配器",

  "keymap.layer": "第 {layer} 层",
  "keymap.layers": "{layers} 层 · 每层 {keys} 键",
  "keymap.key": "按键 {index} · {label}",
  "keymap.keyAt": "按键 {index}，第 {layer} 层：",
  "keymap.apply": "应用",
  "keymap.cancel": "取消",
  "keymap.note":
    "写入整个键位图：适配器只发送改动的键码，读回校验，若键盘不一致则恢复原始镜像。修改前的镜像保存在 %APPDATA%\\UniversalPeripheralHub\\keymap-backups。",

  "keycode.transparent": "透明",
  "keycode.none": "无 (KC_NO)",
  "keycode.enter": "回车",
  "keycode.escape": "Esc",
  "keycode.backspace": "退格",
  "keycode.tab": "Tab",
  "keycode.space": "空格",
  "keycode.capsLock": "大写锁定",
  "keycode.printScreen": "截屏",
  "keycode.scrollLock": "滚动锁定",
  "keycode.pause": "暂停",
  "keycode.insert": "插入",
  "keycode.home": "Home",
  "keycode.pageUp": "上翻页",
  "keycode.delete": "删除",
  "keycode.end": "End",
  "keycode.pageDown": "下翻页",
  "keycode.numLock": "数字锁定",
  "keycode.mute": "静音",
  "keycode.volumeDown": "音量 −",
  "keycode.volumeUp": "音量 +",

  "battery.tooltip": "{label} · {percent}%",

  "settings.title": "设置",
  "settings.language": "语言",
  "settings.closeBehaviour": "关闭窗口时",
  "settings.hideToTray": "隐藏到托盘",
  "settings.quitApp": "退出应用",
  "settings.autoScan": "自动扫描设备",
  "settings.autoScanInterval": "扫描间隔",
  "settings.interval": "{seconds} 秒",
  "settings.batteryAlert": "低电量提醒",
  "settings.batteryThreshold": "低于此电量时提醒（%）",
  "settings.save": "保存",
  "settings.cancel": "取消",
  "settings.loading": "正在加载设置…",
  "settings.loadFailed": "无法加载设置：{error}",
  "settings.saveFailed": "无法保存设置：{error}",
};

const zhTW: Record<Key, string> = {
  "app.title": "Universal Peripheral Hub",
  "app.subtitle": "一個行程，統管顯示、輸入、音訊等周邊。",
  "app.connected": "hubd 已連線",
  "app.disconnected": "hubd 無回應",
  "app.rescan": "重新掃描",
  "app.settings": "設定",

  "offline.title": "無法連線 hubd",
  "offline.body": "常駐核心（hubd）沒有回應。它會由面板與系統匣圖示自動啟動。",
  "offline.retry": "重試",

  "error.actionFailed": "操作失敗",
  "error.unknown": "未知錯誤",

  "adapters.title": "轉接器",
  "adapters.enable": "啟用",
  "adapters.disable": "停用",
  "adapters.deviceCount": "{count} 臺裝置",
  "adapters.disabled": "已停用",
  "adapters.builtIn": "內建",
  "adapters.subprocess": "子行程",
  "adapters.ignoredCount": "已忽略 {count}",
  "adapters.ignoredTitle": "在 {adapter} 上有 {count} 個被忽略的裝置權杖",
  "adapters.restore": "恢復",
  "adapters.restoreTitle": "重新與 {token} 通訊。",

  "scenes.title": "場景",
  "scenes.empty": "尚無場景。它們儲存在 %APPDATA%\\UniversalPeripheralHub\\scenes.json。",
  "scenes.current": "目前",
  "scenes.selectedByRule": "由目前規則選中。",
  "scenes.apply": "套用",
  "scenes.applyTitle": "立即套用「{name}」。",
  "scenes.valuesCount": "{count} 個值",
  "scenes.noItems": "此場景沒有任何項目。",
  "scenes.baselineMissing": "尚未記錄基準（不會寫入任何內容）。",
  "scenes.appliedManually": "手動套用：{name}",
  "scenes.autoMatch": "自動：{exe} → {name}",
  "scenes.autoNoMatch": "自動：沒有規則符合 {exe}",
  "scenes.report": "寫入 {written} · 未變 {unchanged} · 略過 {skipped}",
  "scenes.moreIssues": "{first} 等另外 {count} 項",
  "scenes.allowSideEffects": "允許會短暫黑屏的變更（HDR、影像模式）",
  "scenes.applyAtStartup": "啟動時套用",

  "devices.loadingTitle": "正在尋找裝置…",
  "devices.emptyTitle": "沒有裝置",
  "devices.loadingBody": "正在向每個已啟用的轉接器詢問裝置清單。",
  "devices.emptyBody": "所有轉接器皆已停用，或沒有轉接器回報裝置。",
  "devices.ignore": "忽略",
  "devices.ignoreTitle": "停止與此顯示器通訊。適用於會卡住 DDC 匯流排的 KVM 或切換器。",

  "card.readOnly": "唯讀",
  "card.renegotiate": "可能重新協商訊號",
  "card.writeOnly": "唯寫",
  "card.reading": "讀取中…",
  "card.readBack": "已從裝置讀回",
  "card.rawBytes": "原始位元組",
  "card.bytes": "{count} 位元組",
  "card.latency": "{ms} ms",

  "control.on": "開",
  "control.off": "關",

  "status.saving": "儲存中…",
  "status.synced": "已同步 · 讀回 {value}",
  "status.readbackFailed": "已套用，但讀回失敗：{error}",
  "status.deviceKept": "裝置保留了 {value}",
  "status.retrying": "因 {kind} 重試…",

  "confirm.ignore":
    "忽略「{name}」嗎？\n\n停止與此顯示器通訊。適用於會卡住 DDC 匯流排的 KVM 或切換器。\n\n之後可在轉接器一欄中恢復。",

  "footer.note": "裝置卡片由轉接器能力描述產生。",
  "footer.milestone": "M0 · 模擬轉接器",

  "keymap.layer": "第 {layer} 層",
  "keymap.layers": "{layers} 層 · 每層 {keys} 鍵",
  "keymap.key": "按鍵 {index} · {label}",
  "keymap.keyAt": "按鍵 {index}，第 {layer} 層：",
  "keymap.apply": "套用",
  "keymap.cancel": "取消",
  "keymap.note":
    "寫入整個鍵位圖：轉接器只傳送變更的鍵碼，讀回驗證，若鍵盤不一致則還原原始映射。修改前的映射儲存在 %APPDATA%\\UniversalPeripheralHub\\keymap-backups。",

  "keycode.transparent": "透明",
  "keycode.none": "無 (KC_NO)",
  "keycode.enter": "Enter",
  "keycode.escape": "Esc",
  "keycode.backspace": "退格",
  "keycode.tab": "Tab",
  "keycode.space": "空白鍵",
  "keycode.capsLock": "大寫鎖定",
  "keycode.printScreen": "截圖",
  "keycode.scrollLock": "捲動鎖定",
  "keycode.pause": "暫停",
  "keycode.insert": "插入",
  "keycode.home": "Home",
  "keycode.pageUp": "上一頁",
  "keycode.delete": "刪除",
  "keycode.end": "End",
  "keycode.pageDown": "下一頁",
  "keycode.numLock": "數字鎖定",
  "keycode.mute": "靜音",
  "keycode.volumeDown": "音量 −",
  "keycode.volumeUp": "音量 +",

  "battery.tooltip": "{label} · {percent}%",

  "settings.title": "設定",
  "settings.language": "語言",
  "settings.closeBehaviour": "關閉視窗時",
  "settings.hideToTray": "隱藏到系統匣",
  "settings.quitApp": "結束應用程式",
  "settings.autoScan": "自動掃描裝置",
  "settings.autoScanInterval": "掃描間隔",
  "settings.interval": "{seconds} 秒",
  "settings.batteryAlert": "低電量提醒",
  "settings.batteryThreshold": "低於此電量時提醒（%）",
  "settings.save": "儲存",
  "settings.cancel": "取消",
  "settings.loading": "正在載入設定…",
  "settings.loadFailed": "無法載入設定：{error}",
  "settings.saveFailed": "無法儲存設定：{error}",
};

const DICTIONARIES: Record<Language, Record<Key, string>> = {
  en,
  "zh-CN": zhCN,
  "zh-TW": zhTW,
};

/** Languages offered by the settings page, in menu order. */
export const LANGUAGE_OPTIONS: { id: Language; label: string }[] = [
  { id: "en", label: "English" },
  { id: "zh-CN", label: "简体中文" },
  { id: "zh-TW", label: "繁體中文" },
];

let language: Language = "en";

/** Narrow an arbitrary string to a language this module actually ships. */
export function isLanguage(value: string): value is Language {
  return value === "en" || value === "zh-CN" || value === "zh-TW";
}

/**
 * Translate one namespaced key.
 *
 * A key missing from the active language falls back to English, then to the
 * key itself, so a half-translated build degrades to readable English instead
 * of throwing. `{name}` placeholders are replaced from `vars`; unknown
 * placeholders are left untouched.
 */
export function t(key: string, vars?: Record<string, string | number>): string {
  const active = DICTIONARIES[language] as Record<string, string | undefined>;
  const fallback = en as Record<string, string | undefined>;
  const template = active[key] ?? fallback[key];
  if (template === undefined) return key;
  if (!vars) return template;
  return template.replace(/\{(\w+)\}/g, (match, name: string) => {
    const value = vars[name];
    return value === undefined ? match : String(value);
  });
}

/** Switch the active dictionary. The caller is responsible for one repaint. */
export function setLanguage(next: Language): void {
  if (!isLanguage(next)) return;
  language = next;
  document.documentElement.lang = next;
}

export function getLanguage(): Language {
  return language;
}
