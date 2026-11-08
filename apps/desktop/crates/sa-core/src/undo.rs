//! Undo stack of named groups (win-core §7; the same algorithm serves macOS's NSUndoManager semantics,
//! UNIFIED.md §3.5). UI thread only.
//!
//! Re-entrancy: actions may register into the stack while it runs them (registrations made by an undo action form
//! the inverse group pushed onto the opposite stack). Actions therefore receive `&mut UndoStack<C>` plus the
//! context.

use crate::subscription::{Subscribers, Subscription};

/// An undo action over a context `C` (the view-model state).
pub type UndoAction<C> = Box<dyn FnOnce(&mut C, &mut UndoStack<C>)>;

/// A named group; the **last** registered key names it. Keys are catalog keys localized when shown
/// (`Konuşmacıyı Değiştir`, `Metni Değiştir`, `Satırları Birleştir`, `Satırı Böl`, `Satırı Sil`, `Satırları Sil`,
/// `Zamanlamayı Değiştir`, `Çeviriyi Değiştir`, `Seçilenleri Yeniden Çevir`).
pub struct UndoGroup<C> {
    pub action_key: String,
    pub actions: Vec<UndoAction<C>>,
}

/// The stack.
pub struct UndoStack<C> {
    undo: Vec<UndoGroup<C>>,
    redo: Vec<UndoGroup<C>>,
    open: Option<UndoGroup<C>>,
    depth: u32,
    is_undoing: bool,
    is_redoing: bool,
    changed: Subscribers<()>,
}

impl<C> Default for UndoStack<C> {
    fn default() -> Self {
        Self::new()
    }
}

impl<C> UndoStack<C> {
    pub fn new() -> Self {
        Self {
            undo: Vec::new(),
            redo: Vec::new(),
            open: None,
            depth: 0,
            is_undoing: false,
            is_redoing: false,
            changed: Subscribers::new(),
        }
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }
    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }
    pub fn undo_action_key(&self) -> Option<&str> {
        self.undo.last().map(|g| g.action_key.as_str())
    }
    pub fn redo_action_key(&self) -> Option<&str> {
        self.redo.last().map(|g| g.action_key.as_str())
    }
    pub fn is_undoing(&self) -> bool {
        self.is_undoing
    }
    pub fn is_redoing(&self) -> bool {
        self.is_redoing
    }

    /// `depth += 1`; pair with [`end_group`](Self::end_group) (nestable).
    pub fn begin_group(&mut self) {
        self.depth += 1;
    }

    /// Closes a group; the outermost close pushes the open group (C# `GroupScope.Dispose` → `EndGroup`).
    pub fn end_group(&mut self) {
        if self.depth == 0 {
            return;
        }
        self.depth -= 1;
        if self.depth > 0 || self.is_undoing || self.is_redoing || self.open.is_none() {
            return;
        }
        if let Some(group) = self.open.take() {
            self.push(group);
        }
    }

    /// `begin_group`, `f`, `end_group` (also when `f` panics).
    pub fn with_group<R>(&mut self, f: impl FnOnce(&mut Self) -> R) -> R {
        self.begin_group();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(self)));
        self.end_group();
        match result {
            Ok(r) => r,
            Err(p) => std::panic::resume_unwind(p),
        }
    }

    /// Registers an undo action under `action_key` (win-core §7 `register`): into the open group (whose name becomes
    /// `action_key`), else a new group that is opened while inside `begin_group` (and not undoing/redoing), else
    /// pushed at once.
    pub fn register(&mut self, action_key: &str, action: UndoAction<C>) {
        if let Some(open) = self.open.as_mut() {
            open.actions.push(action);
            open.action_key = action_key.to_owned();
            return;
        }
        let group = UndoGroup { action_key: action_key.to_owned(), actions: vec![action] };
        if self.depth > 0 && !self.is_undoing && !self.is_redoing {
            self.open = Some(group);
            return;
        }
        self.push(group);
    }

    /// Runs the top undo group in reverse registration order; its registrations become the redo group.
    pub fn undo(&mut self, ctx: &mut C) {
        self.run(ctx, true);
    }

    /// Runs the top redo group.
    pub fn redo(&mut self, ctx: &mut C) {
        self.run(ctx, false);
    }

    /// Empties both stacks and the open group (depth unchanged); fires `changed`.
    pub fn clear(&mut self) {
        self.undo.clear();
        self.redo.clear();
        self.open = None;
        self.changed.notify(&());
    }

    fn push(&mut self, group: UndoGroup<C>) {
        if self.is_undoing {
            self.redo.push(group);
        } else if self.is_redoing {
            self.undo.push(group);
        } else {
            self.undo.push(group);
            self.redo.clear();
        }
        self.changed.notify(&());
    }

    /// win-core §7 `run`: registrations made by the actions land in the inverse group (named like the group that
    /// ran), pushed onto the opposite stack without clearing anything; `changed` always fires.
    fn run(&mut self, ctx: &mut C, undoing: bool) {
        if self.is_undoing || self.is_redoing {
            return;
        }
        let group = if undoing { self.undo.pop() } else { self.redo.pop() };
        let Some(group) = group else { return };
        let action_key = group.action_key;
        let saved_open = self.open.replace(UndoGroup { action_key: action_key.clone(), actions: Vec::new() });
        if undoing {
            self.is_undoing = true;
        } else {
            self.is_redoing = true;
        }
        let mut actions = group.actions;
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            while let Some(action) = actions.pop() {
                action(ctx, self);
            }
        }));
        self.is_undoing = false;
        self.is_redoing = false;
        let inverse = std::mem::replace(&mut self.open, saved_open);
        if let Err(p) = result {
            std::panic::resume_unwind(p);
        }
        if let Some(mut inverse) = inverse
            && !inverse.actions.is_empty()
        {
            inverse.action_key = action_key;
            if undoing {
                self.redo.push(inverse);
            } else {
                self.undo.push(inverse);
            }
        }
        self.changed.notify(&());
    }

    /// `changed` event (no payload).
    pub fn subscribe(&self, handler: impl Fn(&()) + Send + Sync + 'static) -> Subscription {
        self.changed.subscribe(handler)
    }
}

/// `L("%@ İşlemini Geri Al", L(key))` or `L("Geri Al")` (Windows button tooltip).
pub fn undo_title(action_key: Option<&str>) -> String {
    match action_key {
        Some(k) => crate::l!("%@ İşlemini Geri Al", crate::l10n::l(k)),
        None => crate::l10n::l("Geri Al"),
    }
}

/// `L("%@ İşlemini Yinele", L(key))` or `L("Yinele")` (Windows button tooltip).
pub fn redo_title(action_key: Option<&str>) -> String {
    match action_key {
        Some(k) => crate::l!("%@ İşlemini Yinele", crate::l10n::l(k)),
        None => crate::l10n::l("Yinele"),
    }
}

/// macOS Edit menu title: `Geri Al: %@` / `Geri Al`.
pub fn mac_undo_menu_title(action_key: Option<&str>) -> String {
    match action_key {
        Some(k) => crate::l!("Geri Al: %@", crate::l10n::l(k)),
        None => crate::l10n::l("Geri Al"),
    }
}

/// macOS Edit menu title: `Yinele: %@` / `Yinele`.
pub fn mac_redo_menu_title(action_key: Option<&str>) -> String {
    match action_key {
        Some(k) => crate::l!("Yinele: %@", crate::l10n::l(k)),
        None => crate::l10n::l("Yinele"),
    }
}
