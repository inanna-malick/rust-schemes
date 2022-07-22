use crate::functor::Functor;
use crate::recursive::{Foldable, Generatable};
use crate::recursive_tree::RecursiveTree;
use crate::recursive_tree::block_allocation::ArenaIndex;

/// A linked list of characters. Not good or idiomatic, but it provides a nice minimal example
#[derive(Debug, Clone, Copy)]
pub enum CharLinkedList<A> {
    Cons(char, A),
    Nil,
}

impl<A, B> Functor<B> for CharLinkedList<A> {
    type To = CharLinkedList<B>;
    type Unwrapped = A;

    fn fmap<F: FnMut(Self::Unwrapped) -> B>(self, mut f: F) -> Self::To {
        match self {
            CharLinkedList::Cons(c, a) => CharLinkedList::Cons(c, f(a)),
            CharLinkedList::Nil => CharLinkedList::Nil,
        }
    }
}

pub type RecursiveString = RecursiveTree<CharLinkedList<ArenaIndex>, ArenaIndex>;

pub fn from_str(s: &str) -> RecursiveString {
    RecursiveString::generate_layers(s.chars(), |mut it| {
        if let Some(c) = it.next() {
            CharLinkedList::Cons(c, it)
        } else {
            CharLinkedList::Nil
        }
    })
}

pub fn to_str(r: RecursiveString) -> String {
    r.fold_layers(|cll| match cll {
        CharLinkedList::Cons(c, s) => format!("{}{}", c, s),
        CharLinkedList::Nil => String::new(),
    })
}
