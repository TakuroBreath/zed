use std::sync::Arc;

use editor::{Editor, EditorEvent, EditorMode};
use gpui::{
    App, ClipboardItem, Context, Entity, EventEmitter, FocusHandle, Focusable, SharedString,
    Subscription, Window,
};
use language::{Buffer, LanguageRegistry};
use multi_buffer::MultiBuffer;
use ui::{Tooltip, prelude::*};

/// Просьбы окна к владельцу (TableDataView): владелец держит `EditTarget` и
/// буфер правок, окно знает только текст.
#[derive(Clone, Debug, PartialEq)]
pub enum ValueViewerEvent {
    /// Забуферизовать текст как новое значение ячейки и закрыть окно.
    Apply(String),
    /// Забуферизовать NULL и закрыть окно.
    SetNull,
    /// Закрыть окно, ничего не записывая.
    Dismissed,
}

pub struct ValueViewerParams {
    pub column: String,
    /// Объявленный тип колонки (например "jsonb"), когда известен.
    pub column_type: Option<String>,
    /// Текущее отображаемое значение ячейки; `None` — NULL. База сравнения
    /// «без изменений» для Apply.
    pub original: Option<String>,
    /// Стартовый текст редактора вместо `original` (правка, перенесённая из
    /// инлайн-редактора). Совпадающий с `original` — игнорируется, чтобы
    /// сработал автоформат.
    pub initial_text: Option<String>,
    /// Тип колонки объявлен как json/jsonb.
    pub declared_json: bool,
    pub read_only: bool,
}

/// Валидный JSON-документ (объект или массив). Голые скаляры ("42", "hello")
/// намеренно не считаются: числовая колонка в кастомном запросе не должна
/// получать JSON-обвязку.
fn is_json_document(text: &str) -> bool {
    matches!(
        serde_json::from_str::<serde_json::Value>(text),
        Ok(serde_json::Value::Object(_)) | Ok(serde_json::Value::Array(_))
    )
}

fn pretty_json(text: &str) -> Option<String> {
    let value = serde_json::from_str::<serde_json::Value>(text).ok()?;
    serde_json::to_string_pretty(&value).ok()
}

fn json_parse_error(text: &str) -> Option<String> {
    match serde_json::from_str::<serde_json::Value>(text) {
        Ok(_) => None,
        Err(error) => Some(error.to_string()),
    }
}

/// `true`, если `source` и `formatted` отличаются только JSON-незначащими
/// пробелами (вне строковых литералов). Нужно, потому что round-trip через
/// `serde_json` нормализует числовые токены: `"1.50"` → `1.5`, `"5.00"` → `5.0`,
/// огромные целые теряют точность, превращаясь во float. Такое
/// «форматирование» молча меняет само значение, поэтому автоформат и кнопка
/// Format предлагаются только когда pretty-форма отличается исключительно
/// расстановкой пробелов — корректность важнее красоты.
fn differs_only_in_whitespace(source: &str, formatted: &str) -> bool {
    // Внутри строкового литерала значим каждый символ (включая пробелы);
    // снаружи пробелы (только JSON-набор ` \t\n\r`) пропускаются.
    fn next_significant(
        chars: &mut std::str::Chars,
        in_string: &mut bool,
        escaped: &mut bool,
    ) -> Option<char> {
        for ch in chars.by_ref() {
            if *in_string {
                if *escaped {
                    *escaped = false;
                } else if ch == '\\' {
                    *escaped = true;
                } else if ch == '"' {
                    *in_string = false;
                }
                return Some(ch);
            }
            if matches!(ch, ' ' | '\t' | '\n' | '\r') {
                continue;
            }
            if ch == '"' {
                *in_string = true;
            }
            return Some(ch);
        }
        None
    }

    let mut source_chars = source.chars();
    let mut formatted_chars = formatted.chars();
    let mut source_in_string = false;
    let mut source_escaped = false;
    let mut formatted_in_string = false;
    let mut formatted_escaped = false;
    loop {
        let source_char = next_significant(
            &mut source_chars,
            &mut source_in_string,
            &mut source_escaped,
        );
        let formatted_char = next_significant(
            &mut formatted_chars,
            &mut formatted_in_string,
            &mut formatted_escaped,
        );
        match (source_char, formatted_char) {
            (None, None) => return true,
            (Some(left), Some(right)) if left == right => continue,
            _ => return false,
        }
    }
}

pub struct ValueViewer {
    editor: Entity<Editor>,
    column: SharedString,
    column_type: Option<SharedString>,
    /// Значение ячейки на момент открытия (`None` — NULL).
    original: Option<String>,
    /// Pretty-print `original`, когда тот парсится как JSON.
    pretty_original: Option<String>,
    is_json: bool,
    read_only: bool,
    /// Ошибка парсинга текущего текста; `None` — валиден. Имеет смысл только
    /// при `is_json` и непустом тексте.
    json_error: Option<String>,
    /// Кэш для render: текущий текст можно безопасно отформатировать (pretty-форма
    /// отличается только пробелами). Пересчитывается в `refresh_validity`, чтобы
    /// render не парсил JSON на каждый кадр.
    can_format: bool,
    _editor_subscription: Subscription,
}

impl EventEmitter<ValueViewerEvent> for ValueViewer {}

impl Focusable for ValueViewer {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.editor.focus_handle(cx)
    }
}

impl ValueViewer {
    pub fn new(
        params: ValueViewerParams,
        language_registry: Option<Arc<LanguageRegistry>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let raw = params.original.clone().unwrap_or_default();
        let is_json =
            params.declared_json || params.original.as_deref().is_some_and(is_json_document);
        // Автоформат при открытии — только когда pretty-форма отличается от исходной
        // строки исключительно пробелами вне строк. Иначе (числовая нормализация
        // serde) значение открывается как есть, а Raw/Format недоступны.
        let pretty_original = is_json
            .then(|| pretty_json(&raw))
            .flatten()
            .filter(|pretty| differs_only_in_whitespace(&raw, pretty));
        let initial_text = match params.initial_text {
            Some(text) if text != raw => text,
            _ => pretty_original.clone().unwrap_or_else(|| raw.clone()),
        };

        let editor = cx.new(|cx| {
            let buffer = cx.new(|cx| {
                let buffer = Buffer::local(initial_text.clone(), cx);
                if let Some(language_registry) = language_registry.clone() {
                    buffer.set_language_registry(language_registry);
                }
                buffer
            });
            let buffer = cx.new(|cx| MultiBuffer::singleton(buffer, cx));
            let mut editor = Editor::new(EditorMode::full(), buffer, None, window, cx);
            if params.read_only {
                editor.set_read_only(true);
            }
            editor
        });

        if is_json && let Some(language_registry) = language_registry {
            cx.spawn(async move |this, cx| {
                let json = language_registry.language_for_name("JSON").await.ok();
                if json.is_none() {
                    log::debug!("JSON language unavailable; value viewer stays plain text");
                }
                // Закрытие окна до резолва языка освобождает entity — ожидаемая
                // гонка, не ошибка.
                this.update(cx, |this: &mut Self, cx| {
                    if let Some(buffer) = this.editor.read(cx).buffer().read(cx).as_singleton() {
                        buffer.update(cx, |buffer, cx| buffer.set_language(json, cx));
                    }
                })
                .ok();
            })
            .detach();
        }

        let _editor_subscription = cx.subscribe(&editor, |this: &mut Self, _, event, cx| {
            if matches!(event, EditorEvent::BufferEdited) {
                this.refresh_validity(cx);
                cx.notify();
            }
        });

        let mut viewer = Self {
            editor,
            column: params.column.into(),
            column_type: params.column_type.map(SharedString::from),
            original: params.original,
            pretty_original,
            is_json,
            read_only: params.read_only,
            json_error: None,
            can_format: false,
            _editor_subscription,
        };
        viewer.refresh_validity(cx);
        viewer
    }

    pub fn editor_text(&self, cx: &App) -> String {
        self.editor.read(cx).text(cx)
    }

    pub fn read_only(&self) -> bool {
        self.read_only
    }

    pub fn is_json(&self) -> bool {
        self.is_json
    }

    pub fn json_error(&self) -> Option<&str> {
        self.json_error.as_deref()
    }

    pub fn can_format(&self) -> bool {
        self.can_format
    }

    fn refresh_validity(&mut self, cx: &App) {
        let text = self.editor_text(cx);
        // Пустой текст (NULL-ячейка) — не «невалидный JSON», индикатор молчит.
        self.json_error = if self.is_json && !text.is_empty() {
            json_parse_error(&text)
        } else {
            None
        };
        // Форматировать безопасно, только если pretty-форма меняет один лишь
        // пробельный layout: числовая нормализация serde исключается.
        self.can_format = pretty_json(&text)
            .is_some_and(|pretty| pretty != text && differs_only_in_whitespace(&text, &pretty));
    }

    /// Текст не отредактирован: совпадает с одной из двух базовых форм
    /// исходного значения. Пока это так, Raw может переключать вид, ничего не
    /// теряя.
    fn raw_available(&self, cx: &App) -> bool {
        let text = self.editor_text(cx);
        let raw = self.original.clone().unwrap_or_default();
        self.pretty_original.is_some()
            && (text == raw || Some(&text) == self.pretty_original.as_ref())
    }

    fn show_raw(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.raw_available(cx) {
            return;
        }
        let raw = self.original.clone().unwrap_or_default();
        self.editor.update(cx, |editor, cx| {
            editor.set_text(raw, window, cx);
        });
        self.refresh_validity(cx);
    }

    fn format_current(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.editor_text(cx);
        let Some(pretty) = pretty_json(&text) else {
            return;
        };
        // Та же защита, что при открытии: не даём числовой нормализации serde
        // подменить значение под видом форматирования.
        if !differs_only_in_whitespace(&text, &pretty) {
            return;
        }
        self.editor.update(cx, |editor, cx| {
            editor.set_text(pretty, window, cx);
        });
        self.refresh_validity(cx);
    }

    fn copy_value(&self, cx: &mut Context<Self>) {
        cx.write_to_clipboard(ClipboardItem::new_string(self.editor_text(cx)));
    }

    pub fn apply(&mut self, cx: &mut Context<Self>) {
        if self.read_only {
            return;
        }
        let text = self.editor_text(cx);
        let unchanged = match &self.original {
            Some(raw) => text == *raw || Some(&text) == self.pretty_original.as_ref(),
            None => text.is_empty(),
        };
        if unchanged {
            cx.emit(ValueViewerEvent::Dismissed);
        } else {
            cx.emit(ValueViewerEvent::Apply(text));
        }
    }

    #[cfg(test)]
    pub fn set_editor_text(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.editor.update(cx, |editor, cx| {
            editor.set_text(text.to_string(), window, cx);
        });
        self.refresh_validity(cx);
    }
}

impl Render for ValueViewer {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let text = self.editor_text(cx);
        let raw_available = self.raw_available(cx);
        let can_format = self.can_format;
        let is_null = self.original.is_none() && text.is_empty();

        let validity = (self.is_json && !text.is_empty()).then(|| match &self.json_error {
            None => Label::new("Valid JSON")
                .size(LabelSize::Small)
                .color(Color::Success)
                .into_any_element(),
            Some(error) => div()
                .max_w_96()
                .overflow_hidden()
                .child(
                    Label::new(format!("Invalid JSON: {error}"))
                        .size(LabelSize::Small)
                        .color(Color::Error)
                        .single_line(),
                )
                .into_any_element(),
        });

        v_flex()
            .key_context("DatabaseValueViewer")
            .occlude()
            .elevation_3(cx)
            .w(gpui::relative(0.65))
            .h(gpui::relative(0.75))
            .overflow_hidden()
            .rounded_md()
            .bg(cx.theme().colors().elevated_surface_background)
            .on_action(
                cx.listener(|_, _: &menu::Cancel, _, cx| cx.emit(ValueViewerEvent::Dismissed)),
            )
            .on_mouse_down_out(cx.listener(|_, _, _, cx| cx.emit(ValueViewerEvent::Dismissed)))
            .child(
                h_flex()
                    .px_2()
                    .py_1()
                    .gap_2()
                    .items_center()
                    .justify_between()
                    .border_b_1()
                    .border_color(cx.theme().colors().border)
                    .child(
                        h_flex()
                            .gap_2()
                            .items_center()
                            .child(Label::new(self.column.clone()))
                            .when_some(self.column_type.clone(), |this, column_type| {
                                this.child(
                                    Label::new(column_type)
                                        .size(LabelSize::Small)
                                        .color(Color::Muted),
                                )
                            })
                            .when(is_null, |this| {
                                this.child(
                                    Label::new("NULL")
                                        .size(LabelSize::Small)
                                        .color(Color::Muted)
                                        .italic(),
                                )
                            }),
                    )
                    .when(self.read_only, |this| {
                        this.child(
                            Label::new("read-only")
                                .size(LabelSize::Small)
                                .color(Color::Muted),
                        )
                    }),
            )
            .child(
                div()
                    .flex_1()
                    .overflow_hidden()
                    .bg(cx.theme().colors().editor_background)
                    .child(self.editor.clone()),
            )
            .child(
                h_flex()
                    .px_2()
                    .py_1()
                    .gap_1()
                    .items_center()
                    .justify_between()
                    .border_t_1()
                    .border_color(cx.theme().colors().border)
                    .child(h_flex().children(validity))
                    .child(
                        h_flex()
                            .gap_1()
                            .child(
                                Button::new("value-viewer-format", "Format")
                                    .size(ButtonSize::Compact)
                                    .style(ButtonStyle::Subtle)
                                    .disabled(!can_format)
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.format_current(window, cx)
                                    })),
                            )
                            .child(
                                Button::new("value-viewer-raw", "Raw")
                                    .size(ButtonSize::Compact)
                                    .style(ButtonStyle::Subtle)
                                    .disabled(!raw_available)
                                    .tooltip(Tooltip::text("Show the original, unformatted value"))
                                    .on_click(
                                        cx.listener(|this, _, window, cx| {
                                            this.show_raw(window, cx)
                                        }),
                                    ),
                            )
                            .child(
                                Button::new("value-viewer-copy", "Copy")
                                    .size(ButtonSize::Compact)
                                    .style(ButtonStyle::Subtle)
                                    .on_click(cx.listener(|this, _, _, cx| this.copy_value(cx))),
                            )
                            .when(!self.read_only, |this| {
                                this.child(
                                    Button::new("value-viewer-null", "∅ NULL")
                                        .size(ButtonSize::Compact)
                                        .style(ButtonStyle::Subtle)
                                        .tooltip(Tooltip::text("Set this cell to NULL"))
                                        .on_click(cx.listener(|_, _, _, cx| {
                                            cx.emit(ValueViewerEvent::SetNull)
                                        })),
                                )
                                .child(
                                    Button::new("value-viewer-apply", "Apply")
                                        .size(ButtonSize::Compact)
                                        .style(ButtonStyle::Filled)
                                        .on_click(cx.listener(|this, _, _, cx| this.apply(cx))),
                                )
                            })
                            .child(
                                Button::new("value-viewer-cancel", "Cancel")
                                    .size(ButtonSize::Compact)
                                    .style(ButtonStyle::Subtle)
                                    .on_click(cx.listener(|_, _, _, cx| {
                                        cx.emit(ValueViewerEvent::Dismissed)
                                    })),
                            ),
                    ),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::TestAppContext;
    use std::cell::RefCell;
    use std::rc::Rc;

    fn init_test(cx: &mut TestAppContext) {
        cx.update(|cx| {
            let settings_store = settings::SettingsStore::test(cx);
            cx.set_global(settings_store);
            theme_settings::init(theme::LoadThemes::JustBase, cx);
            editor::init(cx);
            gpui_tokio::init(cx);
        });
    }

    fn viewer_params(original: Option<&str>) -> ValueViewerParams {
        ValueViewerParams {
            column: "payload".into(),
            column_type: None,
            original: original.map(str::to_string),
            initial_text: None,
            declared_json: false,
            read_only: false,
        }
    }

    #[gpui::test]
    async fn json_object_pretty_printed_on_open(cx: &mut TestAppContext) {
        init_test(cx);
        let cx = cx.add_empty_window();
        let viewer = cx.update(|window, cx| {
            cx.new(|cx| ValueViewer::new(viewer_params(Some(r#"{"a":1}"#)), None, window, cx))
        });
        viewer.read_with(cx, |viewer, cx| {
            assert_eq!(viewer.editor_text(cx), "{\n  \"a\": 1\n}");
            assert!(viewer.is_json());
            assert!(viewer.json_error().is_none());
        });
    }

    #[gpui::test]
    async fn scalar_value_not_treated_as_json(cx: &mut TestAppContext) {
        init_test(cx);
        let cx = cx.add_empty_window();
        let viewer = cx.update(|window, cx| {
            cx.new(|cx| ValueViewer::new(viewer_params(Some("42")), None, window, cx))
        });
        viewer.read_with(cx, |viewer, cx| {
            assert_eq!(viewer.editor_text(cx), "42");
            assert!(
                !viewer.is_json(),
                "a bare scalar must not get JSON affordances"
            );
        });
    }

    #[gpui::test]
    async fn declared_json_column_reports_error_for_invalid_text(cx: &mut TestAppContext) {
        init_test(cx);
        let cx = cx.add_empty_window();
        let mut params = viewer_params(Some("{oops"));
        params.declared_json = true;
        let viewer =
            cx.update(|window, cx| cx.new(|cx| ValueViewer::new(params, None, window, cx)));
        viewer.read_with(cx, |viewer, cx| {
            assert_eq!(
                viewer.editor_text(cx),
                "{oops",
                "invalid JSON must stay raw"
            );
            assert!(viewer.is_json());
            assert!(viewer.json_error().is_some());
        });
    }

    #[gpui::test]
    async fn raw_toggle_restores_original_and_is_disabled_after_edit(cx: &mut TestAppContext) {
        init_test(cx);
        let cx = cx.add_empty_window();
        let viewer = cx.update(|window, cx| {
            cx.new(|cx| ValueViewer::new(viewer_params(Some(r#"{"a":1}"#)), None, window, cx))
        });
        viewer.update_in(cx, |viewer, window, cx| {
            assert!(viewer.raw_available(cx));
            viewer.show_raw(window, cx);
            assert_eq!(viewer.editor_text(cx), r#"{"a":1}"#);
            viewer.format_current(window, cx);
            assert_eq!(viewer.editor_text(cx), "{\n  \"a\": 1\n}");
            viewer.set_editor_text("{\"a\": 2}", window, cx);
            assert!(!viewer.raw_available(cx), "Raw must not clobber user edits");
        });
    }

    #[gpui::test]
    async fn apply_unchanged_dismisses_and_changed_applies(cx: &mut TestAppContext) {
        init_test(cx);
        let cx = cx.add_empty_window();
        let viewer = cx.update(|window, cx| {
            cx.new(|cx| ValueViewer::new(viewer_params(Some(r#"{"a":1}"#)), None, window, cx))
        });
        let events: Rc<RefCell<Vec<ValueViewerEvent>>> = Rc::default();
        let _subscription = cx.update(|_, cx| {
            let events = events.clone();
            cx.subscribe(&viewer, move |_, event: &ValueViewerEvent, _| {
                events.borrow_mut().push(event.clone());
            })
        });
        // Открылось pretty-print'нутым и не редактировалось — Apply закрывает без записи.
        viewer.update(cx, |viewer, cx| viewer.apply(cx));
        assert_eq!(events.borrow().as_slice(), [ValueViewerEvent::Dismissed]);
        events.borrow_mut().clear();

        viewer.update_in(cx, |viewer, window, cx| {
            viewer.set_editor_text("{\"a\": 2}", window, cx);
            viewer.apply(cx);
        });
        assert_eq!(
            events.borrow().as_slice(),
            [ValueViewerEvent::Apply("{\"a\": 2}".into())]
        );
    }

    #[gpui::test]
    async fn null_original_with_empty_text_dismisses_on_apply(cx: &mut TestAppContext) {
        init_test(cx);
        let cx = cx.add_empty_window();
        let viewer = cx.update(|window, cx| {
            cx.new(|cx| ValueViewer::new(viewer_params(None), None, window, cx))
        });
        let events: Rc<RefCell<Vec<ValueViewerEvent>>> = Rc::default();
        let _subscription = cx.update(|_, cx| {
            let events = events.clone();
            cx.subscribe(&viewer, move |_, event: &ValueViewerEvent, _| {
                events.borrow_mut().push(event.clone());
            })
        });
        viewer.read_with(cx, |viewer, cx| assert_eq!(viewer.editor_text(cx), ""));
        viewer.update(cx, |viewer, cx| viewer.apply(cx));
        assert_eq!(events.borrow().as_slice(), [ValueViewerEvent::Dismissed]);
    }

    #[gpui::test]
    async fn initial_text_override_skips_autoformat(cx: &mut TestAppContext) {
        init_test(cx);
        let cx = cx.add_empty_window();
        let mut params = viewer_params(Some(r#"{"a":1}"#));
        // Текст из инлайн-редактора: правка в процессе, форматировать нельзя.
        params.initial_text = Some(r#"{"a":2"#.into());
        let viewer =
            cx.update(|window, cx| cx.new(|cx| ValueViewer::new(params, None, window, cx)));
        viewer.read_with(cx, |viewer, cx| {
            assert_eq!(viewer.editor_text(cx), r#"{"a":2"#);
        });
    }

    #[gpui::test]
    async fn read_only_viewer_ignores_apply(cx: &mut TestAppContext) {
        init_test(cx);
        let cx = cx.add_empty_window();
        let mut params = viewer_params(Some("hello"));
        params.read_only = true;
        let viewer =
            cx.update(|window, cx| cx.new(|cx| ValueViewer::new(params, None, window, cx)));
        let events: Rc<RefCell<Vec<ValueViewerEvent>>> = Rc::default();
        let _subscription = cx.update(|_, cx| {
            let events = events.clone();
            cx.subscribe(&viewer, move |_, event: &ValueViewerEvent, _| {
                events.borrow_mut().push(event.clone());
            })
        });
        viewer.update(cx, |viewer, cx| {
            assert!(viewer.read_only());
            viewer.apply(cx);
        });
        assert!(
            events.borrow().is_empty(),
            "read-only Apply must be a no-op"
        );
    }

    #[gpui::test]
    async fn numeric_scalar_opens_raw_with_format_disabled(cx: &mut TestAppContext) {
        init_test(cx);
        let cx = cx.add_empty_window();
        // `numeric(10,2)` рендерится как "1.50"; serde бы нормализовал его в "1.5".
        let viewer = cx.update(|window, cx| {
            cx.new(|cx| ValueViewer::new(viewer_params(Some("1.50")), None, window, cx))
        });
        viewer.read_with(cx, |viewer, cx| {
            assert_eq!(
                viewer.editor_text(cx),
                "1.50",
                "scalar must open exactly as stored"
            );
            assert!(!viewer.is_json());
            assert!(
                !viewer.raw_available(cx),
                "no safe pretty form → Raw disabled"
            );
            assert!(
                !viewer.can_format(),
                "reformatting would drop the trailing zero"
            );
        });
    }

    #[gpui::test]
    async fn json_document_with_normalizing_number_opens_raw(cx: &mut TestAppContext) {
        init_test(cx);
        let cx = cx.add_empty_window();
        // Валидный JSON, но pretty-print сменил бы 1.50 на 1.5 — открываем как есть.
        let viewer = cx.update(|window, cx| {
            cx.new(|cx| {
                ValueViewer::new(viewer_params(Some(r#"{"amount":1.50}"#)), None, window, cx)
            })
        });
        viewer.read_with(cx, |viewer, cx| {
            assert_eq!(
                viewer.editor_text(cx),
                r#"{"amount":1.50}"#,
                "number-normalizing document must not autoformat"
            );
            assert!(viewer.is_json());
            assert!(
                viewer.json_error().is_none(),
                "the raw document is still valid JSON"
            );
            assert!(!viewer.raw_available(cx));
            assert!(!viewer.can_format());
        });
    }

    #[test]
    fn differs_only_in_whitespace_semantics() {
        // Форматирование, меняющее только раскладку пробелов вне строк.
        assert!(differs_only_in_whitespace(r#"{"a":1}"#, "{\n  \"a\": 1\n}"));
        assert!(differs_only_in_whitespace(r#"{"a":1}"#, r#"{"a":1}"#));

        // Числовая нормализация — это уже не только пробелы.
        assert!(!differs_only_in_whitespace("1.50", "1.5"));
        assert!(!differs_only_in_whitespace(
            r#"{"amount":1.50}"#,
            r#"{"amount":1.5}"#
        ));

        // Пробелы внутри строкового литерала значимы.
        assert!(differs_only_in_whitespace(
            r#"{"a":"x y"}"#,
            "{\n  \"a\": \"x y\"\n}"
        ));
        assert!(!differs_only_in_whitespace(
            r#"{"a":"x y"}"#,
            r#"{"a":"xy"}"#
        ));

        // Экранированная кавычка не закрывает строку раньше времени.
        let escaped = r#"{"a":"x\"y"}"#;
        let pretty = pretty_json(escaped).expect("valid JSON");
        assert!(differs_only_in_whitespace(escaped, &pretty));
        assert!(!differs_only_in_whitespace(escaped, r#"{"a":"xy"}"#));
    }
}
