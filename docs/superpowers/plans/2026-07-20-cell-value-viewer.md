# Cell Value Viewer/Editor Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Большое центрированное окно просмотра/редактирования значения одной ячейки (json/jsonb/text) в гриде БД-панели: настоящий редактор Zed с подсветкой JSON, автоформат, индикатор валидности, Apply через существующий буфер правок.

**Architecture:** Новый компонент `ValueViewer` (крейт `database_ui`, новый файл `value_viewer.rs`) — GPUI-entity со встроенным `Editor`, знающая только текст и излучающая события `Apply/SetNull/Dismissed`. `TableDataView` владеет состоянием открытого окна, привязкой к `EditTarget` и пишет в буфер правок. Две точки входа: пункт контекстного меню «View value» (заменяет старый мини-попап `ValuePopover`, который удаляется) и кнопка «развернуть» в инлайн-редакторе.

**Tech Stack:** Rust, GPUI, крейты `editor`/`language`/`multi_buffer` (уже в зависимостях `database_ui`), `serde_json` (добавляется).

**Spec:** `docs/superpowers/specs/2026-07-20-cell-value-viewer-design.md` — прочитать перед началом.

## Global Constraints

- Спека — источник истины; при конфликте плана и спеки сообщить контролёру, не решать самостоятельно.
- Не использовать `unwrap()`/panicking-индексацию; ошибки — через `?`, `.log_err()`, или явный `match` (правило репозитория, CLAUDE.md).
- Комментарии только «почему», не «что»; полные слова в именах переменных.
- Кнопка **Raw** активна только пока текст редактора совпадает с одной из двух базовых форм исходного значения (raw или pretty) — переключение видов никогда не затирает правки.
- База сравнения «без изменений» для Apply — **исходное значение ячейки** (`original`), не текст на момент открытия окна.
- Невалидный JSON не блокирует Apply.
- Apply не делает прямой `UPDATE` — только буферизация через существующие `set_cell_value`/`set_cell_null`/`set_new_cell_value`/`set_new_cell_null`.
- Read-only режим окна, когда: `!editable()`, или `target == None`, или колонка — часть первичного ключа.
- Кеймап-блок `"context": "DatabaseValueViewer > Editor"` с `"escape": "menu::Cancel"` добавляется во все три файла: `assets/keymaps/default-macos.json`, `default-linux.json`, `default-windows.json` (в windows-файле с `"use_key_equivalents": true`), рядом с существующим блоком `SqlQueryEditor > Editor`.
- Линт: `./script/clippy -p database_ui` (не `cargo clippy`).
- Тесты: `cargo test -p database_ui`.
- Коммиты в стиле репозитория (`database_ui: ...`), **без** trailer'а `Co-Authored-By: Claude` — явное требование пользователя этого форка.
- Не пушить в origin.

---

### Task 1: Компонент `ValueViewer` (новый файл + serde_json)

**Files:**
- Create: `crates/database_ui/src/value_viewer.rs`
- Modify: `crates/database_ui/Cargo.toml` (добавить `serde_json.workspace = true` в `[dependencies]`)
- Modify: `crates/database_ui/src/database_ui.rs` (добавить `pub mod value_viewer;` рядом с остальными `mod`)
- Test: тесты внутри `crates/database_ui/src/value_viewer.rs` (`#[cfg(test)] mod tests`)

**Interfaces:**
- Consumes: `editor::{Editor, EditorEvent, EditorMode}`, `language::{Buffer, LanguageRegistry}`, `multi_buffer::MultiBuffer` — паттерн встраивания редактора скопировать из `crates/database_ui/src/sql_query_view.rs:71-110` (создание Buffer → MultiBuffer → Editor, асинхронное назначение языка через `language_registry.language_for_name(...)`).
- Produces (Task 2 и 3 полагаются на эти сигнатуры):
  - `pub struct ValueViewerParams { pub column: String, pub column_type: Option<String>, pub original: Option<String>, pub initial_text: Option<String>, pub declared_json: bool, pub read_only: bool }`
  - `impl ValueViewer { pub fn new(params: ValueViewerParams, language_registry: Option<Arc<LanguageRegistry>>, window: &mut Window, cx: &mut Context<Self>) -> Self }`
  - `#[derive(Clone, Debug, PartialEq)] pub enum ValueViewerEvent { Apply(String), SetNull, Dismissed }` + `impl EventEmitter<ValueViewerEvent> for ValueViewer {}`
  - `pub fn editor_text(&self, cx: &App) -> String`
  - `pub fn apply(&mut self, cx: &mut Context<Self>)` (излучает `Apply`/`Dismissed` по правилу «без изменений»)
  - `pub fn read_only(&self) -> bool`
  - `#[cfg(test)] pub fn set_editor_text(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>)`
  - `impl Focusable for ValueViewer` (возвращает focus handle встроенного редактора)

- [ ] **Step 1: Добавить serde_json и зарегистрировать модуль**

В `crates/database_ui/Cargo.toml` в `[dependencies]` (по алфавиту, после `settings`):

```toml
serde_json.workspace = true
```

В `crates/database_ui/src/database_ui.rs` рядом с существующими объявлениями модулей:

```rust
pub mod value_viewer;
```

- [ ] **Step 2: Написать падающие тесты**

Создать `crates/database_ui/src/value_viewer.rs` сразу с тестовым модулем (реализация — в Step 4; для компиляции тестов нужны объявления типов, поэтому нормально писать skeleton типов вместе с тестами, но логика — только после того, как тесты видимо падают). Тестовая обвязка повторяет `table_data_view.rs::tests::init_test` (`SettingsStore::test`, `theme_settings::init`, `editor::init`, `gpui_tokio::init`) без `crate::init` и с `cx.add_empty_window()`; `LanguageRegistry` в тестах не передаём (`None`) — язык остаётся плоским, как в тестах `sql_query_view`.

```rust
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
            assert!(!viewer.is_json(), "a bare scalar must not get JSON affordances");
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
            assert_eq!(viewer.editor_text(cx), "{oops", "invalid JSON must stay raw");
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
        let viewer = cx
            .update(|window, cx| cx.new(|cx| ValueViewer::new(viewer_params(None), None, window, cx)));
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
        assert!(events.borrow().is_empty(), "read-only Apply must be a no-op");
    }
}
```

Для компиляции тестов добавить в `[dev-dependencies]` `Cargo.toml` ничего не нужно — `theme_settings`, `editor/test-support`, `gpui/test-support` уже там.

- [ ] **Step 3: Убедиться, что тесты падают**

Run: `cargo test -p database_ui value_viewer 2>&1 | tail -20`
Expected: ошибки компиляции «cannot find … ValueViewer» (реализация ещё не написана) — это и есть «красный» этап при skeleton-подходе; допустимо написать пустые типы и увидеть падения assert'ов.

- [ ] **Step 4: Реализация**

Верх файла `crates/database_ui/src/value_viewer.rs` (до тестового модуля):

```rust
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
        let pretty_original = pretty_json(&raw);
        let initial_text = match params.initial_text {
            Some(text) if text != raw => text,
            _ => pretty_original.clone().unwrap_or_else(|| raw.clone()),
        };
        let is_json = params.declared_json
            || params
                .original
                .as_deref()
                .is_some_and(is_json_document);

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

    fn refresh_validity(&mut self, cx: &App) {
        let text = self.editor_text(cx);
        // Пустой текст (NULL-ячейка) — не «невалидный JSON», индикатор молчит.
        self.json_error = if self.is_json && !text.is_empty() {
            json_parse_error(&text)
        } else {
            None
        };
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
    }

    fn format_current(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(pretty) = pretty_json(&self.editor_text(cx)) else {
            return;
        };
        self.editor.update(cx, |editor, cx| {
            editor.set_text(pretty, window, cx);
        });
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
```

Рендер (там же). Раскладка: шапка (колонка + тип + NULL-бейдж), редактор с `flex_1`, футер (индикатор слева, кнопки справа):

```rust
impl Render for ValueViewer {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let text = self.editor_text(cx);
        let raw_available = self.raw_available(cx);
        let can_format = pretty_json(&text).is_some_and(|pretty| pretty != text);
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
            .on_action(cx.listener(|_, _: &menu::Cancel, _, cx| {
                cx.emit(ValueViewerEvent::Dismissed)
            }))
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
                                    .tooltip(Tooltip::text(
                                        "Show the original, unformatted value",
                                    ))
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.show_raw(window, cx)
                                    })),
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
```

Примечания для исполнителя:
- `if is_json && let Some(...)` — let-chains уже используются в кодебазе (`table_data_view.rs:2144`); если компилятор этого издания откажет, заменить на вложенный `if`.
- Если `gpui::relative` не подойдёт как `Length` для `.w()/.h()`, использовать `.w_2_3().h_3_4()` либо фиксированные `rems` с `max_w_full()/max_h_full()` — выбрать то, что компилируется, визуальная точность тут не критична.
- `raw_available`/`show_raw`/`format_current` вызываются из тестов — пометить `pub(crate)` или оставить приватными и звать через тесты того же модуля (тесты в том же файле — приватность не мешает).

- [ ] **Step 5: Прогнать тесты**

Run: `cargo test -p database_ui value_viewer 2>&1 | tail -5`
Expected: все тесты `value_viewer::tests::*` PASS.

- [ ] **Step 6: Clippy и коммит**

Run: `./script/clippy -p database_ui 2>&1 | tail -5` — без ошибок.

```bash
git add crates/database_ui/Cargo.toml crates/database_ui/src/database_ui.rs crates/database_ui/src/value_viewer.rs Cargo.lock
git commit -m "database_ui: Add ValueViewer component for cell value inspection"
```

---

### Task 2: Интеграция в TableDataView (замена ValuePopover) + кеймапы

**Files:**
- Modify: `crates/database_ui/src/table_data_view.rs`
- Modify: `assets/keymaps/default-macos.json`, `assets/keymaps/default-linux.json`, `assets/keymaps/default-windows.json`
- Test: тесты в `crates/database_ui/src/table_data_view.rs`

**Interfaces:**
- Consumes (из Task 1): `crate::value_viewer::{ValueViewer, ValueViewerEvent, ValueViewerParams}`; `ValueViewer::new(params, language_registry, window, cx)`; `viewer.editor_text(cx)`, `viewer.apply(cx)`, `viewer.read_only()`, `#[cfg(test)] viewer.set_editor_text(...)`.
- Consumes (уже в файле): `set_cell_value(row_key, column, value, cx)`, `set_cell_null(row_key, column, cx)`, `set_new_cell_value(id, column, value, cx)`, `set_new_cell_null(id, column, cx)`, `editable()`, `is_saving()`, `is_primary_key_column(&column)`, `EditTarget`, `self.structure`, `self.query.base` / `QueryBase::Table(_)`.
- Produces (Task 3 полагается): `pub fn open_value_viewer(&mut self, column: String, value: Option<String>, target: Option<EditTarget>, initial_text: Option<String>, window: &mut Window, cx: &mut Context<Self>)`; `#[cfg(test)] fn value_viewer_open(&self) -> bool`; `#[cfg(test)] fn value_viewer(&self) -> Option<&Entity<ValueViewer>>`.

- [ ] **Step 1: Написать падающие тесты**

Удалить два старых теста попапа: `view_value_populates_popover_with_full_text` и `view_value_shows_null_literal_for_null_cell` (около строки 6764). На их месте — новые (используют существующие фикстуры `init_test`, `fake_with_default_rows`, `view_with_default_rows`, `wait_until`, `RowKey`):

```rust
    #[gpui::test]
    async fn view_value_opens_viewer_with_pretty_json(cx: &mut TestAppContext) {
        let cx = &mut cx.clone();
        let (_fake, view, cx) = view_with_default_rows(cx).await;

        let key = RowKey {
            columns: vec!["id".into()],
            values: vec![Some("1".into())],
        };
        view.update_in(cx, |view, window, cx| {
            view.open_value_viewer(
                "name".into(),
                Some(r#"{"a":1}"#.into()),
                Some(EditTarget::Existing(key)),
                None,
                window,
                cx,
            );
        });
        view.read_with(cx, |view, cx| {
            assert!(view.value_viewer_open());
            let viewer = view.value_viewer().expect("viewer should be open");
            assert_eq!(viewer.read(cx).editor_text(cx), "{\n  \"a\": 1\n}");
            assert!(!viewer.read(cx).read_only());
        });
    }

    #[gpui::test]
    async fn viewer_apply_buffers_update(cx: &mut TestAppContext) {
        let cx = &mut cx.clone();
        let (_fake, view, cx) = view_with_default_rows(cx).await;

        let key = RowKey {
            columns: vec!["id".into()],
            values: vec![Some("1".into())],
        };
        view.update_in(cx, |view, window, cx| {
            view.open_value_viewer(
                "name".into(),
                Some("Alice".into()),
                Some(EditTarget::Existing(key.clone())),
                None,
                window,
                cx,
            );
        });
        let viewer = view
            .read_with(cx, |view, _| view.value_viewer().cloned())
            .expect("viewer should be open");
        viewer.update_in(cx, |viewer, window, cx| {
            viewer.set_editor_text("Alicia", window, cx);
            viewer.apply(cx);
        });
        view.read_with(cx, |view, _| {
            assert!(!view.value_viewer_open(), "apply closes the viewer");
            let cell = view
                .edits()
                .updates()
                .get(&key)
                .and_then(|row| row.get("name"))
                .expect("the applied value should be buffered");
            assert_eq!(cell, &database_client::EditCell::Value("Alicia".into()));
        });
    }

    #[gpui::test]
    async fn viewer_apply_unchanged_closes_without_buffering(cx: &mut TestAppContext) {
        let cx = &mut cx.clone();
        let (_fake, view, cx) = view_with_default_rows(cx).await;

        let key = RowKey {
            columns: vec!["id".into()],
            values: vec![Some("1".into())],
        };
        view.update_in(cx, |view, window, cx| {
            view.open_value_viewer(
                "name".into(),
                Some("Alice".into()),
                Some(EditTarget::Existing(key)),
                None,
                window,
                cx,
            );
        });
        let viewer = view
            .read_with(cx, |view, _| view.value_viewer().cloned())
            .expect("viewer should be open");
        viewer.update(cx, |viewer, cx| viewer.apply(cx));
        view.read_with(cx, |view, _| {
            assert!(!view.value_viewer_open());
            assert_eq!(view.pending_change_count(), 0, "unchanged apply must not buffer");
        });
    }

    #[gpui::test]
    async fn viewer_set_null_buffers_null(cx: &mut TestAppContext) {
        let cx = &mut cx.clone();
        let (_fake, view, cx) = view_with_default_rows(cx).await;

        let key = RowKey {
            columns: vec!["id".into()],
            values: vec![Some("1".into())],
        };
        view.update_in(cx, |view, window, cx| {
            view.open_value_viewer(
                "name".into(),
                Some("Alice".into()),
                Some(EditTarget::Existing(key.clone())),
                None,
                window,
                cx,
            );
        });
        let viewer = view
            .read_with(cx, |view, _| view.value_viewer().cloned())
            .expect("viewer should be open");
        viewer.update(cx, |_, cx| cx.emit(crate::value_viewer::ValueViewerEvent::SetNull));
        view.read_with(cx, |view, _| {
            assert!(!view.value_viewer_open());
            let cell = view
                .edits()
                .updates()
                .get(&key)
                .and_then(|row| row.get("name"))
                .expect("NULL should be buffered");
            assert_eq!(cell, &database_client::EditCell::Null);
        });
    }

    #[gpui::test]
    async fn viewer_without_target_is_read_only(cx: &mut TestAppContext) {
        let cx = &mut cx.clone();
        let (_fake, view, cx) = view_with_default_rows(cx).await;

        view.update_in(cx, |view, window, cx| {
            view.open_value_viewer("name".into(), Some("Alice".into()), None, None, window, cx);
        });
        view.read_with(cx, |view, cx| {
            let viewer = view.value_viewer().expect("viewer should be open");
            assert!(viewer.read(cx).read_only());
        });
    }

    #[gpui::test]
    async fn viewer_on_primary_key_column_is_read_only(cx: &mut TestAppContext) {
        let cx = &mut cx.clone();
        let (_fake, view, cx) = view_with_default_rows(cx).await;

        let key = RowKey {
            columns: vec!["id".into()],
            values: vec![Some("1".into())],
        };
        view.update_in(cx, |view, window, cx| {
            view.open_value_viewer(
                "id".into(),
                Some("1".into()),
                Some(EditTarget::Existing(key)),
                None,
                window,
                cx,
            );
        });
        view.read_with(cx, |view, cx| {
            let viewer = view.value_viewer().expect("viewer should be open");
            assert!(viewer.read(cx).read_only(), "PK identifies the row; editing it is forbidden");
        });
    }

    #[gpui::test]
    async fn viewer_null_cell_opens_empty(cx: &mut TestAppContext) {
        let cx = &mut cx.clone();
        let (_fake, view, cx) = view_with_default_rows(cx).await;

        let key = RowKey {
            columns: vec!["id".into()],
            values: vec![Some("3".into())],
        };
        view.update_in(cx, |view, window, cx| {
            view.open_value_viewer(
                "name".into(),
                None,
                Some(EditTarget::Existing(key)),
                None,
                window,
                cx,
            );
        });
        view.read_with(cx, |view, cx| {
            let viewer = view.value_viewer().expect("viewer should be open");
            assert_eq!(viewer.read(cx).editor_text(cx), "");
        });
    }

    #[gpui::test]
    async fn viewer_detects_declared_json_column(cx: &mut TestAppContext) {
        init_test(cx);
        cx.executor().allow_parking();
        let mut fake = FakeDatabaseClient::new();
        fake.query_result = default_rows_result();
        if let Some(name_column) = fake
            .structure
            .columns
            .iter_mut()
            .find(|column| column.name == "name")
        {
            name_column.data_type = "jsonb".into();
            name_column.udt_name = "jsonb".into();
        }
        let client: Arc<dyn DatabaseClient> = Arc::new(fake);

        let cx = cx.add_empty_window();
        let view = cx.update(|window, cx| {
            TableDataView::new(client, "local".into(), table_ref(), false, None, window, cx)
        });
        wait_until(cx, |cx| {
            view.read_with(cx, |view, _| {
                view.page().is_some() && view.structure().is_some()
            })
        })
        .await;

        let key = RowKey {
            columns: vec!["id".into()],
            values: vec![Some("1".into())],
        };
        view.update_in(cx, |view, window, cx| {
            view.open_value_viewer(
                "name".into(),
                Some("{broken".into()),
                Some(EditTarget::Existing(key)),
                None,
                window,
                cx,
            );
        });
        view.read_with(cx, |view, cx| {
            let viewer = view.value_viewer().expect("viewer should be open");
            assert!(
                viewer.read(cx).is_json(),
                "a declared jsonb column gets JSON affordances even for invalid content"
            );
            assert!(viewer.read(cx).json_error().is_some());
        });
    }
```

Если фикстуры `view_with_default_rows` нет (есть только `fake_with_default_rows` + ручная сборка) — собрать вью так же, как это делает `commit_cell_edit_buffers_update` (см. `table_data_view.rs:5288`), и оформить локальный helper по образцу соседних тестов.

- [ ] **Step 2: Убедиться, что тесты падают**

Run: `cargo test -p database_ui viewer 2>&1 | tail -10`
Expected: ошибки компиляции — `open_value_viewer`, `value_viewer_open` не существуют.

- [ ] **Step 3: Реализация в table_data_view.rs**

3a. Импорты: добавить `use crate::value_viewer::{ValueViewer, ValueViewerEvent, ValueViewerParams};` к существующим `use crate::...`.

3b. Поля: удалить поле `value_popover: Option<(Entity<ValuePopover>, Point<Pixels>, Subscription)>` (строка ~406) и его инициализацию `value_popover: None` в `new()`. Добавить:

```rust
    /// Открытое окно просмотра/редактирования значения ячейки, вместе с целью
    /// записи, захваченной при открытии (см. `open_value_viewer`).
    value_viewer: Option<ValueViewerState>,
    /// Нужен окну значения для подсветки JSON; `None` в тестах.
    language_registry: Option<Arc<LanguageRegistry>>,
```

и структуру рядом с `EditingCell`:

```rust
/// Состояние открытого окна значения: сам viewer плюс цель записи. Цель
/// захватывается при открытии по стабильному ключу (`RowKey`/`InsertId`), а не
/// по позиции на экране, поэтому Apply ложится в нужную строку, даже если
/// отображение сменилось (тот же принцип, что у инлайн-редактора).
struct ValueViewerState {
    viewer: Entity<ValueViewer>,
    target: Option<EditTarget>,
    column: String,
    _subscription: Subscription,
}
```

В `new()` инициализировать `value_viewer: None` и `language_registry: language_registry.clone()` — параметр `language_registry` уже приходит в `new()`; клонировать его ДО перемещения в задачу назначения SQL-языка (передвинуть строку при необходимости).

3c. Удалить `ValuePopover` целиком (struct + `EventEmitter` + `Focusable` + `Render`, строки ~3306-3349), метод `open_value_popover` (~2225) и тест-хелпер `value_popover_text` (~579). Удалить рендер попапа в `render()` — второй блок `deferred(anchored()...)` (~3430), который рисует `value_popover`.

3d. Новые методы (рядом с бывшим `open_value_popover`):

```rust
    /// Тип колонки объявлен как json/jsonb. Только в табличном режиме: в
    /// кастомном запросе колонки результата не совпадают со структурой таблицы.
    fn column_is_json(&self, column: &str) -> bool {
        matches!(self.query.base, QueryBase::Table(_))
            && self.structure.as_ref().is_some_and(|structure| {
                structure.columns.iter().any(|info| {
                    info.name == column && matches!(info.udt_name.as_str(), "json" | "jsonb")
                })
            })
    }

    fn column_type_label(&self, column: &str) -> Option<String> {
        if !matches!(self.query.base, QueryBase::Table(_)) {
            return None;
        }
        let structure = self.structure.as_ref()?;
        structure
            .columns
            .iter()
            .find(|info| info.name == column)
            .map(|info| info.data_type.clone())
    }

    /// Открывает окно просмотра/редактирования значения ячейки. `value` — текущее
    /// отображаемое значение (`None` — NULL), `initial_text` — правка,
    /// перенесённая из инлайн-редактора. Окно read-only, когда записать некуда
    /// или нельзя: нет цели, вью нередактируемо, или колонка — часть первичного
    /// ключа.
    pub fn open_value_viewer(
        &mut self,
        column: String,
        value: Option<String>,
        target: Option<EditTarget>,
        initial_text: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let read_only =
            !self.editable() || target.is_none() || self.is_primary_key_column(&column);
        let params = ValueViewerParams {
            column: column.clone(),
            column_type: self.column_type_label(&column),
            original: value,
            initial_text,
            declared_json: self.column_is_json(&column),
            read_only,
        };
        let language_registry = self.language_registry.clone();
        let viewer = cx.new(|cx| ValueViewer::new(params, language_registry, window, cx));
        window.focus(&viewer.focus_handle(cx), cx);
        let subscription = cx.subscribe(&viewer, |this, _, event: &ValueViewerEvent, cx| {
            let Some(state) = this.value_viewer.take() else {
                return;
            };
            match event {
                ValueViewerEvent::Apply(text) => {
                    this.apply_viewer_value(state.target, &state.column, Some(text.clone()), cx);
                }
                ValueViewerEvent::SetNull => {
                    this.apply_viewer_value(state.target, &state.column, None, cx);
                }
                ValueViewerEvent::Dismissed => {}
            }
            cx.notify();
        });
        self.value_viewer = Some(ValueViewerState {
            viewer,
            target,
            column,
            _subscription: subscription,
        });
        cx.notify();
    }

    /// Кладёт значение (или NULL) из окна значения в буфер правок. Та же
    /// защита, что у `commit_cell_edit_inner`: если редактирование стало
    /// недоступно, пока окно было открыто, ничего не записываем.
    fn apply_viewer_value(
        &mut self,
        target: Option<EditTarget>,
        column: &str,
        value: Option<String>,
        cx: &mut Context<Self>,
    ) {
        if !self.editable() || self.is_saving() {
            return;
        }
        let Some(target) = target else {
            return;
        };
        match (target, value) {
            (EditTarget::Existing(row_key), Some(value)) => {
                self.set_cell_value(row_key, column, value, cx)
            }
            (EditTarget::Existing(row_key), None) => self.set_cell_null(row_key, column, cx),
            (EditTarget::New(id), Some(value)) => self.set_new_cell_value(id, column, value, cx),
            (EditTarget::New(id), None) => self.set_new_cell_null(id, column, cx),
        }
    }
```

(Сверить сигнатуры `set_cell_value` и др. по строкам 706-780 — при расхождении следовать коду, не плану.)

3e. Контекстное меню (~2136): заменить блок `"View value"` на:

```rust
            menu = menu.separator().entry("View value", None, {
                let table_view = table_view.clone();
                let column = column.clone();
                let value = value.clone();
                let target = target.clone();
                move |window, cx| {
                    table_view
                        .update(cx, |table, cx| {
                            table.open_value_viewer(
                                column.clone(),
                                value.clone(),
                                target.clone(),
                                None,
                                window,
                                cx,
                            );
                        })
                        .log_err();
                }
            });
```

Внимание: ниже в билдере меню `column`/`target` используются веткой "Edit cell" — клонировать до move-замыкания, как в существующем коде с `value`. Параметр `position` у "View value" больше не нужен (окно центрировано), но `position` остаётся нужен самому `deploy_cell_context_menu`.

3f. Рендер оверлея: на месте удалённого блока попапа в `render()` добавить (пример существующего блока `deferred(anchored()...)` контекст-меню — ориентир для места вставки):

```rust
            .when_some(self.value_viewer.as_ref(), |this, state| {
                this.child(deferred(
                    div()
                        .occlude()
                        .absolute()
                        .inset_0()
                        .flex()
                        .items_center()
                        .justify_center()
                        .bg(gpui::black().opacity(0.4))
                        .child(state.viewer.clone()),
                ))
            })
```

Закрытие по клику мимо окна обеспечивает `on_mouse_down_out` в самом `ValueViewer` (Task 1); Escape — кеймап (Step 4). `deferred` уже импортирован в файле.

3g. Тест-хелперы (на месте удалённого `value_popover_text`):

```rust
    /// Test-only: открыто ли окно значения.
    #[cfg(test)]
    fn value_viewer_open(&self) -> bool {
        self.value_viewer.is_some()
    }

    /// Test-only: entity открытого окна значения.
    #[cfg(test)]
    fn value_viewer(&self) -> Option<&Entity<ValueViewer>> {
        self.value_viewer.as_ref().map(|state| &state.viewer)
    }
```

- [ ] **Step 4: Кеймапы**

В каждом из трёх файлов рядом с существующим блоком `"context": "SqlQueryEditor > Editor"`:

`assets/keymaps/default-macos.json` и `assets/keymaps/default-linux.json`:

```json
  {
    "context": "DatabaseValueViewer > Editor",
    "bindings": {
      "escape": "menu::Cancel",
    },
  },
```

`assets/keymaps/default-windows.json` (блок с `use_key_equivalents`, как у соседей в этом файле):

```json
  {
    "context": "DatabaseValueViewer > Editor",
    "use_key_equivalents": true,
    "bindings": {
      "escape": "menu::Cancel",
    },
  },
```

Точный стиль запятых/отступов скопировать у соседнего блока в каждом файле.

- [ ] **Step 5: Прогнать тесты**

Run: `cargo test -p database_ui 2>&1 | tail -5`
Expected: все PASS (в т.ч. новые `viewer_*` и старые нетронутые; двух удалённых попап-тестов больше нет).

- [ ] **Step 6: Clippy и коммит**

Run: `./script/clippy -p database_ui 2>&1 | tail -5` — без ошибок.

```bash
git add crates/database_ui/src/table_data_view.rs assets/keymaps/default-macos.json assets/keymaps/default-linux.json assets/keymaps/default-windows.json
git commit -m "database_ui: Open cell values in the value viewer instead of the popover"
```

---

### Task 3: Кнопка «развернуть» в инлайн-редакторе

**Files:**
- Modify: `crates/database_ui/src/table_data_view.rs` (метод `render_cell_editor` ~2370 и новый метод `expand_editing_cell`)
- Test: тесты в `crates/database_ui/src/table_data_view.rs`

**Interfaces:**
- Consumes (из Task 2): `open_value_viewer(column, value, target, initial_text, window, cx)`, `value_viewer_open()`, `value_viewer()`.
- Consumes (уже в файле): `EditingCell { target, column, original, field, .. }`, `begin_edit_cell`, `editing_cell()`, `pending_change_count()`, `edits()`.
- Produces: `pub fn expand_editing_cell(&mut self, window: &mut Window, cx: &mut Context<Self>)`.

- [ ] **Step 1: Написать падающие тесты**

Рядом с `commit_cell_edit_buffers_update` (использовать её постановку — `init_test`, `fake_with_default_rows`, `add_empty_window`, `wait_until` до `page + structure`):

```rust
    #[gpui::test]
    async fn expand_transfers_inline_text_without_committing(cx: &mut TestAppContext) {
        init_test(cx);
        cx.executor().allow_parking();
        let client: Arc<dyn DatabaseClient> = fake_with_default_rows();

        let cx = cx.add_empty_window();
        let view = cx.update(|window, cx| {
            TableDataView::new(client, "local".into(), table_ref(), false, None, window, cx)
        });
        wait_until(cx, |cx| {
            view.read_with(cx, |view, _| {
                view.page().is_some() && view.structure().is_some()
            })
        })
        .await;

        view.update_in(cx, |view, window, cx| {
            view.begin_edit_cell(0, 1, window, cx);
            let editing = view.editing_cell().expect("edit should be in progress");
            editing.field.update(cx, |field, cx| {
                field.set_text("Alicia-in-progress", window, cx);
            });
            view.expand_editing_cell(window, cx);
        });

        view.read_with(cx, |view, cx| {
            assert!(view.editing_cell().is_none(), "expand closes the inline editor");
            assert_eq!(view.pending_change_count(), 0, "expand must not commit");
            let viewer = view.value_viewer().expect("expand opens the viewer");
            assert_eq!(viewer.read(cx).editor_text(cx), "Alicia-in-progress");
            assert!(!viewer.read(cx).read_only());
        });
    }

    #[gpui::test]
    async fn expand_then_apply_buffers_the_edited_text(cx: &mut TestAppContext) {
        init_test(cx);
        cx.executor().allow_parking();
        let client: Arc<dyn DatabaseClient> = fake_with_default_rows();

        let cx = cx.add_empty_window();
        let view = cx.update(|window, cx| {
            TableDataView::new(client, "local".into(), table_ref(), false, None, window, cx)
        });
        wait_until(cx, |cx| {
            view.read_with(cx, |view, _| {
                view.page().is_some() && view.structure().is_some()
            })
        })
        .await;

        view.update_in(cx, |view, window, cx| {
            view.begin_edit_cell(0, 1, window, cx);
            let editing = view.editing_cell().expect("edit should be in progress");
            editing.field.update(cx, |field, cx| {
                field.set_text("Alicia", window, cx);
            });
            view.expand_editing_cell(window, cx);
        });
        let viewer = view
            .read_with(cx, |view, _| view.value_viewer().cloned())
            .expect("viewer should be open");
        viewer.update(cx, |viewer, cx| viewer.apply(cx));

        view.read_with(cx, |view, _| {
            let key = RowKey {
                columns: vec!["id".into()],
                values: vec![Some("1".into())],
            };
            let cell = view
                .edits()
                .updates()
                .get(&key)
                .and_then(|row| row.get("name"))
                .expect("the applied value should be buffered");
            assert_eq!(cell, &database_client::EditCell::Value("Alicia".into()));
        });
    }

    #[gpui::test]
    async fn expand_unmodified_then_apply_buffers_nothing(cx: &mut TestAppContext) {
        init_test(cx);
        cx.executor().allow_parking();
        let client: Arc<dyn DatabaseClient> = fake_with_default_rows();

        let cx = cx.add_empty_window();
        let view = cx.update(|window, cx| {
            TableDataView::new(client, "local".into(), table_ref(), false, None, window, cx)
        });
        wait_until(cx, |cx| {
            view.read_with(cx, |view, _| {
                view.page().is_some() && view.structure().is_some()
            })
        })
        .await;

        view.update_in(cx, |view, window, cx| {
            view.begin_edit_cell(0, 1, window, cx);
            view.expand_editing_cell(window, cx);
        });
        let viewer = view
            .read_with(cx, |view, _| view.value_viewer().cloned())
            .expect("viewer should be open");
        viewer.update(cx, |viewer, cx| viewer.apply(cx));

        view.read_with(cx, |view, _| {
            assert!(!view.value_viewer_open());
            assert_eq!(view.pending_change_count(), 0);
        });
    }
```

- [ ] **Step 2: Убедиться, что тесты падают**

Run: `cargo test -p database_ui expand 2>&1 | tail -10`
Expected: ошибка компиляции — `expand_editing_cell` не существует.

- [ ] **Step 3: Реализация**

Метод (рядом с `cancel_cell_edit`):

```rust
    /// Переносит открытый инлайн-редактор в большое окно значения: текст поля
    /// (возможно, уже изменённый) становится стартовым текстом окна, инлайн
    /// закрывается без коммита — базой «без изменений» остаётся исходное
    /// значение ячейки, так что перенесённая правка не потеряется при Apply.
    pub fn expand_editing_cell(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(editing) = self.editing_cell.take() else {
            return;
        };
        let text = editing.field.read(cx).text(cx);
        self.open_value_viewer(
            editing.column.clone(),
            editing.original.clone(),
            Some(editing.target.clone()),
            Some(text),
            window,
            cx,
        );
    }
```

В `render_cell_editor` (~2370) добавить кнопку между полем и NULL-кнопкой:

```rust
            .child(
                IconButton::new("db-cell-expand", IconName::Maximize)
                    .icon_size(IconSize::Small)
                    .tooltip(Tooltip::text("Open in value editor"))
                    .on_click(
                        cx.listener(|this, _, window, cx| this.expand_editing_cell(window, cx)),
                    ),
            )
```

`IconButton`, `IconName`, `IconSize` — из прелюдии `ui`; проверить импорты вверху файла и дополнить при необходимости. `IconName::Maximize` существует (`crates/icons/src/icons.rs:191`).

- [ ] **Step 4: Прогнать тесты**

Run: `cargo test -p database_ui 2>&1 | tail -5`
Expected: все PASS.

- [ ] **Step 5: Clippy и коммит**

Run: `./script/clippy -p database_ui 2>&1 | tail -5` — без ошибок.

```bash
git add crates/database_ui/src/table_data_view.rs
git commit -m "database_ui: Add expand button promoting the inline cell editor to the value viewer"
```

---

## Финальная проверка (контролёр, после всех задач)

1. `cargo test -p database_ui` — весь крейт зелёный.
2. `./script/clippy -p database_ui` — чисто.
3. Финальное whole-branch ревью (skill requesting-code-review), затем пересборка Forge (`scratchpad/build-forge.sh`) и ручная проверка: открыть таблицу с jsonb-колонкой, «View value» → pretty JSON с подсветкой; правка → Apply → Save; кнопка «развернуть» из инлайн-редактора; Escape закрывает окно.
