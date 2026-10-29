//! An OS clipboard read may outlive its UI timeout. Retain its admission until
//! the OS returns, publish at most once, and discard text after permission/focus
//! retirement. Dispatch callbacks outside the caller's state/RefCell borrow.
use super::*;

pub(super) struct Pending {
    pub id: u64,
    pub permitted: ReadPermission,
    pub focused: Box<dyn Fn() -> bool>,
    pub complete: Option<ReadCallback>,
}
pub(super) type Dispatch = (ReadCallback, ClipboardRead);
impl Pending {
    pub fn timeout(&mut self) -> Option<Dispatch> {
        self.complete
            .take()
            .map(|callback| (callback, ClipboardRead::State(ClipboardState::TimedOut)))
    }
    pub fn finish(mut self, read: ClipboardRead) -> Option<Dispatch> {
        let callback = self.complete.take()?;
        let result = if !(self.permitted)() {
            ClipboardRead::State(ClipboardState::Disabled)
        } else if !(self.focused)() {
            ClipboardRead::State(ClipboardState::NotFocused)
        } else {
            read
        };
        Some((callback, result))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{cell::Cell, rc::Rc};
    fn pending(allowed: Rc<Cell<bool>>, focused: Rc<Cell<bool>>) -> (Pending, Rc<Cell<usize>>) {
        let called = Rc::new(Cell::new(0));
        let count = called.clone();
        (
            Pending {
                id: 7,
                permitted: Box::new(move || allowed.get()),
                focused: Box::new(move || focused.get()),
                complete: Some(Box::new(move |_| count.set(count.get() + 1))),
            },
            called,
        )
    }
    #[test]
    fn timeout_keeps_pending_ownership_and_a_late_result_cannot_publish_twice() {
        let (mut request, called) = pending(Rc::new(Cell::new(true)), Rc::new(Cell::new(true)));
        let (callback, result) = request.timeout().unwrap();
        assert_eq!(request.id, 7);
        assert!(request.complete.is_none());
        assert!(request.timeout().is_none());
        callback(result);
        assert!(request
            .finish(ClipboardRead::Text("private clipboard".into()))
            .is_none());
        assert_eq!(called.get(), 1);
    }
    #[test]
    fn permission_and_focus_are_rechecked_before_returning_text() {
        for revoke_permission in [true, false] {
            let allowed = Rc::new(Cell::new(true));
            let focus = Rc::new(Cell::new(true));
            let (request, _) = pending(allowed.clone(), focus.clone());
            if revoke_permission {
                allowed.set(false);
            } else {
                focus.set(false);
            }
            let (_, result) = request
                .finish(ClipboardRead::Text("private clipboard".into()))
                .unwrap();
            assert!(matches!(
                result,
                ClipboardRead::State(ClipboardState::Disabled | ClipboardState::NotFocused)
            ));
        }
    }
}
