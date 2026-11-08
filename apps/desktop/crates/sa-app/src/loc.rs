//! Binds the Slint `Loc` global to `sa_core::l10n` and keeps `Loc.revision` in step with the UI language.

use slint::ComponentHandle;

use sa_core::l10n::{Arg, LanguageState, l, l_args};

use crate::ui::Loc;

/// Installs the lookup callbacks on one window's `Loc` global (every window has its own globals).
pub fn install<C: ComponentHandle>(component: &C)
where
    for<'a> Loc<'a>: slint::Global<'a, C>,
{
    let loc = component.global::<Loc>();
    loc.on_lookup(|_rev, key| l(&key).into());
    loc.on_lookup1(|_rev, key, a| l_args(&key, &[Arg::Str(a.into())]).into());
    loc.on_lookup2(|_rev, key, a, b| l_args(&key, &[Arg::Str(a.into()), Arg::Str(b.into())]).into());
    loc.set_revision(LanguageState::shared().revision() as i32);
}

/// Bumps `Loc.revision` on the UI thread whenever the shared language changes.
pub fn follow_language_changes<C: ComponentHandle + 'static>(weak: slint::Weak<C>)
where
    for<'a> Loc<'a>: slint::Global<'a, C>,
{
    LanguageState::shared()
        .subscribe(move |_| {
            let weak = weak.clone();
            let _ = slint::invoke_from_event_loop(move || {
                if let Some(c) = weak.upgrade() {
                    let rev = LanguageState::shared().revision() as i32;
                    c.global::<Loc>().set_revision(rev);
                }
            });
        })
        .detach();
}
