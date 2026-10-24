//! Browser text entry needs a real editable DOM element. winit's web canvas
//! supplies physical keys, but cannot receive IME or bulk browser insertions.
use egui::{Event, Key, Modifiers};

#[derive(Default)]
struct TextInput {
    events: Vec<Event>,
    modifiers: Modifiers,
    active: bool,
    composing: bool,
    composition_commit: Option<String>,
    escape: bool,
}
impl TextInput {
    fn input(&mut self, value: &str, data: Option<&str>) {
        if !self.active || self.composing {
            return;
        }
        // Browsers differ on whether their final input event precedes or
        // follows compositionend. The composition is committed exactly once.
        if let Some(committed) = self.composition_commit.take() {
            if value == committed || data == Some(committed.as_str()) {
                return;
            }
        }
        if !value.is_empty() {
            self.events.push(Event::Text(value.to_owned()));
        }
    }
    fn composition_start(&mut self) {
        self.composing = true;
        self.composition_commit = None;
    }
    fn composition_end(&mut self, text: &str) {
        self.composing = false;
        if self.active && !text.is_empty() {
            self.events.push(Event::Text(text.to_owned()));
        }
        self.composition_commit = Some(text.to_owned());
    }
    /// True means the browser must not also perform its default edit.
    fn key(&mut self, name: &str, pressed: bool, repeat: bool, modifiers: Modifiers) -> bool {
        if !self.active {
            return false;
        }
        self.modifiers = modifiers;
        if self.composing || name == "Process" {
            return false;
        }
        if pressed {
            self.composition_commit = None;
        }
        let Some(key) = Key::from_name(name) else {
            return false;
        };
        let command = modifiers.command && !modifiers.alt;
        if command && key == Key::V {
            // Let the browser generate a paste event carrying actual clipboard
            // data. Reading egui-winit's in-memory clipboard would paste nothing.
            return false;
        }
        if command && matches!(key, Key::C | Key::X) {
            if pressed {
                self.events.push(if key == Key::C {
                    Event::Copy
                } else {
                    Event::Cut
                });
            }
            return true;
        }
        let navigation = matches!(
            key,
            Key::ArrowLeft
                | Key::ArrowRight
                | Key::ArrowUp
                | Key::ArrowDown
                | Key::Backspace
                | Key::Delete
                | Key::Home
                | Key::End
                | Key::PageUp
                | Key::PageDown
                | Key::Enter
                | Key::Tab
                | Key::Escape
        );
        if navigation || command {
            if key == Key::Escape && pressed {
                self.escape = true;
            }
            self.events.push(Event::Key {
                key,
                physical_key: None,
                pressed,
                repeat,
                modifiers,
            });
            return true;
        }
        // Printable keys, dead keys, and AltGr are handled by DOM input, never
        // by both input and keydown. This also accepts multi-character insertions.
        false
    }
    fn paste(&mut self, text: String) {
        if self.active && !text.is_empty() {
            self.events.push(Event::Paste(text.replace("\r\n", "\n")));
        }
    }
}

#[cfg(target_arch = "wasm32")]
mod browser {
    use super::*;
    use std::{cell::RefCell, rc::Rc};
    use wasm_bindgen::{closure::Closure, JsCast, JsValue};
    use web_sys::{
        ClipboardEvent, CompositionEvent, EventTarget, HtmlTextAreaElement, InputEvent,
        KeyboardEvent,
    };

    type Listener = (
        EventTarget,
        &'static str,
        Closure<dyn FnMut(web_sys::Event)>,
    );
    pub struct WebText {
        element: HtmlTextAreaElement,
        state: Rc<RefCell<TextInput>>,
        listeners: Vec<Listener>,
    }
    impl WebText {
        pub fn new() -> Result<Self, String> {
            let document = web_sys::window().unwrap().document().unwrap();
            let element: HtmlTextAreaElement = document
                .create_element("textarea")
                .map_err(error)?
                .dyn_into()
                .map_err(error)?;
            element.set_id("sauerteig-text-input");
            for (name, value) in [
                ("aria-label", "Active Sauerteig text field"),
                ("autocomplete", "off"),
                ("autocapitalize", "off"),
                ("spellcheck", "false"),
                ("tabindex", "-1"),
            ] {
                element.set_attribute(name, value).map_err(error)?;
            }
            // A focusable transparent element lets the browser own text/IME,
            // while all visible text and selection remain in the egui widget.
            element.set_attribute("style", "position:fixed;left:0;top:0;width:1px;height:1px;opacity:0;pointer-events:none;border:0;padding:0;resize:none;font-size:16px;z-index:-1").map_err(error)?;
            document
                .body()
                .ok_or("Browser document has no body")?
                .append_child(&element)
                .map_err(error)?;
            let mut bridge = Self {
                element,
                state: Rc::new(RefCell::new(TextInput::default())),
                listeners: Vec::new(),
            };
            bridge.install()?;
            Ok(bridge)
        }
        fn listen(
            &mut self,
            name: &'static str,
            mut callback: impl FnMut(web_sys::Event) + 'static,
        ) -> Result<(), String> {
            let target: EventTarget = self.element.clone().into();
            let closure = Closure::<dyn FnMut(web_sys::Event)>::new(move |event| callback(event));
            target
                .add_event_listener_with_callback(name, closure.as_ref().unchecked_ref())
                .map_err(error)?;
            self.listeners.push((target, name, closure));
            Ok(())
        }
        fn install(&mut self) -> Result<(), String> {
            let state = self.state.clone();
            let element = self.element.clone();
            self.listen("input", move |event| {
                let data = event.dyn_ref::<InputEvent>().and_then(|e| e.data());
                let mut state = state.borrow_mut();
                state.input(&element.value(), data.as_deref());
                if !state.composing {
                    element.set_value("");
                }
            })?;
            let state = self.state.clone();
            self.listen("compositionstart", move |_| {
                state.borrow_mut().composition_start();
            })?;
            let state = self.state.clone();
            let element = self.element.clone();
            self.listen("compositionend", move |event| {
                let text = event
                    .dyn_ref::<CompositionEvent>()
                    .and_then(|e| e.data())
                    .unwrap_or_else(|| element.value());
                state.borrow_mut().composition_end(&text);
                element.set_value("");
            })?;
            for (name, pressed) in [("keydown", true), ("keyup", false)] {
                let state = self.state.clone();
                self.listen(name, move |event| {
                    let Some(key) = event.dyn_ref::<KeyboardEvent>() else {
                        return;
                    };
                    // Accept both Ctrl and Cmd shortcuts in browser fields. Read
                    // flags from this event, not winit's later modifier event.
                    let modifiers = Modifiers {
                        alt: key.alt_key(),
                        ctrl: key.ctrl_key(),
                        shift: key.shift_key(),
                        mac_cmd: key.meta_key(),
                        command: key.ctrl_key() || key.meta_key(),
                    };
                    if state
                        .borrow_mut()
                        .key(&key.key(), pressed, key.repeat(), modifiers)
                    {
                        event.prevent_default();
                    }
                    event.stop_propagation();
                })?;
            }
            let state = self.state.clone();
            let element = self.element.clone();
            self.listen("paste", move |event| {
                if let Some(clipboard) = event
                    .dyn_ref::<ClipboardEvent>()
                    .and_then(|e| e.clipboard_data())
                {
                    if let Ok(text) = clipboard.get_data("text/plain") {
                        state.borrow_mut().paste(text);
                    }
                    element.set_value("");
                    event.prevent_default();
                    event.stop_propagation();
                }
            })?;
            for (name, cut) in [("copy", false), ("cut", true)] {
                let state = self.state.clone();
                self.listen(name, move |event| {
                    let mut state = state.borrow_mut();
                    if state.active {
                        state
                            .events
                            .push(if cut { Event::Cut } else { Event::Copy });
                        event.prevent_default();
                        event.stop_propagation();
                    }
                })?;
            }
            let state = self.state.clone();
            self.listen("focus", move |_| {
                state.borrow_mut().events.push(Event::WindowFocused(true));
            })?;
            Ok(())
        }
        pub fn owns_focus(&self) -> bool {
            web_sys::window()
                .and_then(|w| w.document())
                .and_then(|d| d.active_element())
                .is_some_and(|e| e.id() == self.element.id())
        }
        /// Called immediately before egui processes this frame's input.
        pub fn drain(&self, input: &mut egui::RawInput) -> bool {
            let mut state = self.state.borrow_mut();
            if state.active {
                input.modifiers = state.modifiers;
                input.focused = web_sys::window()
                    .and_then(|w| w.document())
                    .is_some_and(|d| d.has_focus().unwrap_or(false));
                // Canvas blur from moving focus into our textarea is internal.
                input
                    .events
                    .retain(|event| !matches!(event, Event::WindowFocused(false)));
            }
            input.events.append(&mut state.events);
            std::mem::take(&mut state.escape)
        }
        pub fn sync(
            &self,
            output: &egui::PlatformOutput,
            enabled: bool,
            canvas: &web_sys::HtmlCanvasElement,
            pixels_per_point: f32,
            scale_factor: f64,
        ) {
            let document = web_sys::window().unwrap().document().unwrap();
            let active = enabled && output.ime.is_some();
            self.state.borrow_mut().active = active;
            if active {
                if let Some(ime) = output.ime {
                    let css = pixels_per_point as f64 / scale_factor;
                    let _ = self.element.style().set_property(
                        "left",
                        &format!("{}px", ime.cursor_rect.left() as f64 * css),
                    );
                    let _ = self.element.style().set_property(
                        "top",
                        &format!("{}px", ime.cursor_rect.bottom() as f64 * css),
                    );
                }
                if document.has_focus().unwrap_or(false) && !self.owns_focus() {
                    let _ = self.element.focus();
                }
            } else {
                let owned = self.owns_focus();
                let mut state = self.state.borrow_mut();
                state.composing = false;
                state.composition_commit = None;
                state.modifiers = Modifiers::default();
                self.element.set_value("");
                if owned {
                    let _ = canvas.focus();
                }
            }
            for command in &output.commands {
                if let egui::OutputCommand::CopyText(text) = command {
                    copy_text(text);
                }
            }
        }
    }
    impl Drop for WebText {
        fn drop(&mut self) {
            for (target, name, callback) in &self.listeners {
                let _ = target
                    .remove_event_listener_with_callback(name, callback.as_ref().unchecked_ref());
            }
            self.element.remove();
        }
    }
    fn error(value: impl Into<JsValue>) -> String {
        format!("Browser text input: {:?}", value.into())
    }
    fn copy_text(text: &str) {
        let navigator = web_sys::window().unwrap().navigator();
        let clipboard = js_sys::Reflect::get(&navigator, &JsValue::from_str("clipboard"))
            .ok()
            .and_then(|value| value.dyn_into::<web_sys::Clipboard>().ok());
        let Some(clipboard) = clipboard else {
            log::warn!("System clipboard is unavailable in this browser context");
            return;
        };
        let promise = clipboard.write_text(text);
        wasm_bindgen_futures::spawn_local(async move {
            if let Err(error) = wasm_bindgen_futures::JsFuture::from(promise).await {
                log::warn!("Clipboard write was denied: {error:?}");
            }
        });
    }
}
#[cfg(target_arch = "wasm32")]
pub use browser::WebText;

#[cfg(test)]
mod tests {
    use super::*;
    fn active() -> TextInput {
        TextInput {
            active: true,
            ..Default::default()
        }
    }
    #[test]
    fn bulk_and_unicode_insert_once_without_keydown_text() {
        let mut input = active();
        assert!(!input.key("W", true, false, Modifiers::default()));
        input.input("Wall pair 🧱", Some("Wall pair 🧱"));
        assert_eq!(input.events, vec![Event::Text("Wall pair 🧱".into())]);
    }
    #[test]
    fn composition_commits_once_for_both_browser_event_orders() {
        for final_input_before_end in [false, true] {
            let mut input = active();
            input.composition_start();
            input.input("に", Some("に"));
            if final_input_before_end {
                input.input("日本", Some("日本"));
            }
            input.composition_end("日本");
            if !final_input_before_end {
                input.input("日本", Some("日本"));
            }
            assert_eq!(input.events, vec![Event::Text("日本".into())]);
            input.key("a", true, false, Modifiers::default());
            input.input("a", Some("a"));
            assert_eq!(input.events.last(), Some(&Event::Text("a".into())));
        }
    }
    #[test]
    fn ctrl_and_cmd_select_all_are_commands_and_paste_uses_browser_data() {
        for modifiers in [
            Modifiers {
                ctrl: true,
                command: true,
                ..Default::default()
            },
            Modifiers {
                mac_cmd: true,
                command: true,
                ..Default::default()
            },
        ] {
            let mut input = active();
            assert!(input.key("a", true, false, modifiers));
            assert!(matches!(
                input.events[0],
                Event::Key {
                    key: Key::A,
                    modifiers: Modifiers { command: true, .. },
                    ..
                }
            ));
            assert!(!input.key("v", true, false, modifiers));
            input.paste("Wall\r\npair".into());
            assert_eq!(
                input.events.last(),
                Some(&Event::Paste("Wall\npair".into()))
            );
        }
    }
    #[test]
    fn inactive_input_does_not_leak_into_game_controls() {
        let mut input = TextInput::default();
        input.input("Wall", None);
        input.paste("Wall".into());
        assert!(!input.key("e", true, false, Modifiers::default()));
        assert!(input.events.is_empty());
    }
}
