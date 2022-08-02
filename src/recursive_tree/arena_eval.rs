//! Recursive structure that uses an arena to quickly collapse recursive structures.

use std::collections::VecDeque;
use std::mem::MaybeUninit;

use futures::future::BoxFuture;
use futures::FutureExt;

use crate::map_layer::MapLayer;
use crate::recursive::{
    Collapse, CollapseWithContext, CollapseWithSubStructure, Expand, ExpandAsync,
};
use crate::recursive_tree::{RecursiveTree, RecursiveTreeRef};

/// Used to mark structures stored in an 'RecursiveTree<Layer<ArenaIndex>, ArenaIndex>'
///
/// Has the same memory cost as a boxed pointer and provides the fastest
/// 'Collapse::collapse_layers' implementation
#[derive(Debug, Clone, Copy)]
pub struct ArenaIndex(usize);

// TODO: can I implement the opposite? append single node to recursive struct?
impl ArenaIndex {
    fn head() -> Self {
        ArenaIndex(0)
    }
}

#[derive(Debug)]
pub struct RecursiveTreeRefWithOffset<'a, Wrapped> {
    recursive_tree: RecursiveTreeRef<'a, Wrapped, ArenaIndex>,
    offset: usize, // arena index offset
}

#[derive(Debug)]
pub struct RecursiveTreeRefWithOffsetAndContext<'a, Wrapped, Cached> {
    recursive_tree: RecursiveTreeRef<'a, Wrapped, ArenaIndex>, // truncated slice w/ head
    offset: usize,                                             // arena index offset
    context: &'a [Option<Cached>], // truncated to same point as recursive tree
}

pub trait Head<'a, Unwrapped> {
    fn head(&'a self) -> Unwrapped;
}

impl<'a, Wrapped, Unwrapped> Head<'a, Unwrapped> for RecursiveTree<Wrapped, ArenaIndex>
where
    &'a Wrapped: MapLayer<RecursiveTreeRefWithOffset<'a, Wrapped>, Unwrapped = ArenaIndex, To = Unwrapped>
        + 'a,
    Unwrapped: 'a,
{
    // self -> Layer<RecursiveTreeRef>
    fn head(&'a self) -> Unwrapped {
        let head = &self.elems[0]; // invariant: always present
        head.map_layer(|idx| RecursiveTreeRefWithOffset {
            recursive_tree: RecursiveTreeRef {
                elems: &self.elems[idx.0..],
                _underlying: std::marker::PhantomData,
            },
            offset: idx.0,
        })
    }
}

impl<A, Underlying, Wrapped> Expand<A, Wrapped> for RecursiveTree<Underlying, ArenaIndex>
where
    Wrapped: MapLayer<ArenaIndex, Unwrapped = A, To = Underlying>,
{
    fn expand_layers<F: Fn(A) -> Wrapped>(a: A, expand_layer: F) -> Self {
        let mut frontier = VecDeque::from([a]);
        let mut elems = vec![];

        // expand to build a vec of elems while preserving topo order
        while let Some(seed) = frontier.pop_front() {
            let layer = expand_layer(seed);

            let layer = layer.map_layer(|aa| {
                frontier.push_back(aa);
                // idx of pointed-to element determined from frontier + elems size
                ArenaIndex(elems.len() + frontier.len())
            });

            elems.push(layer);
        }

        Self {
            elems,
            _underlying: std::marker::PhantomData,
        }
    }
}

impl<A, U: Send, O: MapLayer<ArenaIndex, Unwrapped = A, To = U>> ExpandAsync<A, O>
    for RecursiveTree<U, ArenaIndex>
{
    fn expand_layers_async<
        'a,
        E: Send + 'a,
        F: Fn(A) -> BoxFuture<'a, Result<O, E>> + Send + Sync + 'a,
    >(
        seed: A,
        generate_layer: F,
    ) -> BoxFuture<'a, Result<Self, E>>
    where
        Self: Sized,
        U: Send,
        A: Send + 'a,
    {
        async move {
            let mut frontier = VecDeque::from([seed]);
            let mut elems = vec![];

            // expand to build a vec of elems while preserving topo order
            while let Some(seed) = frontier.pop_front() {
                let layer = generate_layer(seed).await?;

                let layer = layer.map_layer(|aa| {
                    frontier.push_back(aa);
                    // idx of pointed-to element determined from frontier + elems size
                    ArenaIndex(elems.len() + frontier.len())
                });

                elems.push(layer);
            }

            Ok(Self {
                elems,
                _underlying: std::marker::PhantomData,
            })
        }
        .boxed()
    }
}

impl<A, Wrapped, Underlying> Collapse<A, Wrapped> for RecursiveTree<Underlying, ArenaIndex>
where
    Underlying: MapLayer<A, To = Wrapped, Unwrapped = ArenaIndex>,
{
    // TODO: 'checked' compile flag to control whether this gets a vec of maybeuninit or a vec of Option w/ unwrap
    fn collapse_layers<F: FnMut(Wrapped) -> A>(self, mut collapse_layer: F) -> A {
        let mut results = std::iter::repeat_with(|| MaybeUninit::<A>::uninit())
            .take(self.elems.len())
            .collect::<Vec<_>>();

        for (idx, node) in self.elems.into_iter().enumerate().rev() {
            let alg_res = {
                // each node is only referenced once so just remove it, also we know it's there so unsafe is fine
                let node = node.map_layer(|ArenaIndex(x)| unsafe {
                    let maybe_uninit =
                        std::mem::replace(results.get_unchecked_mut(x), MaybeUninit::uninit());
                    maybe_uninit.assume_init()
                });
                collapse_layer(node)
            };
            results[idx].write(alg_res);
        }

        unsafe {
            let maybe_uninit = std::mem::replace(
                results.get_unchecked_mut(ArenaIndex::head().0),
                MaybeUninit::uninit(),
            );
            maybe_uninit.assume_init()
        }
    }
}

// recurse with context-labeled subtree, lol what lmao how does this compile
impl<'a, A, Wrapped, Underlying> CollapseWithSubStructure<'a, A, Wrapped>
    for RecursiveTreeRef<'a, Underlying, ArenaIndex>
where
    for<'x> &'x Underlying: MapLayer<
        (
            &'x A,
            RecursiveTreeRefWithOffsetAndContext<'x, Underlying, A>,
        ),
        To = Wrapped,
        Unwrapped = ArenaIndex,
    >,
    A: 'a,
    Wrapped: 'a, // Layer<(&A, RecursiveTreeRefWithOffsetAndContext)> -> A
    Underlying: 'a,
{
    // TODO: 'checked' compile flag to control whether this gets a vec of maybeuninit or a vec of Option w/ unwrap
    fn collapse_layers_2<F: FnMut(Wrapped) -> A>(&self, mut collapse_layer: F) -> A {
        let mut results: Vec<Option<A>> = std::iter::repeat_with(|| None)
            .take(self.elems.len())
            .collect::<Vec<_>>();

        for (idx, node) in self.elems.iter().enumerate().rev() {
            let alg_res = {
                // each node is only referenced once so just remove it, also we know it's there so unsafe is fine
                let node = node.map_layer(|ArenaIndex(x)| {
                    // TODO: get ref instead of remove

                    let substructure = RecursiveTreeRefWithOffsetAndContext {
                        recursive_tree: RecursiveTreeRef {
                            elems: &self.elems[x..],
                            _underlying: std::marker::PhantomData,
                        },
                        offset: x,
                        context: &results[x..],
                    };

                    (&results[x].as_ref().unwrap(), substructure)
                });
                collapse_layer(node)
            };
            results[idx] = Some(alg_res);
        }

        // doesn't preserve ordering, but at this point we're done and
        // don't care
        let mut maybe = results.swap_remove(ArenaIndex::head().0);
        maybe.take().unwrap()
    }
}

impl<'a, A, O: 'a, U> Collapse<A, O> for RecursiveTreeRef<'a, U, ArenaIndex>
where
    &'a U: MapLayer<A, To = O, Unwrapped = ArenaIndex>,
{
    fn collapse_layers<F: FnMut(O) -> A>(self, collapse_layer: F) -> A {
        RecursiveTreeRefWithOffset {
            recursive_tree: self,
            offset: 0,
        }
        .collapse_layers(collapse_layer)
    }
}

impl<'a, A, O: 'a, U> Collapse<A, O> for RecursiveTreeRefWithOffset<'a, U>
where
    &'a U: MapLayer<A, To = O, Unwrapped = ArenaIndex>,
{
    // TODO: 'checked' compile flag to control whether this gets a vec of maybeuninit or a vec of Option w/ unwrap
    fn collapse_layers<F: FnMut(O) -> A>(self, mut collapse_layer: F) -> A {
        let mut results = std::iter::repeat_with(|| MaybeUninit::<A>::uninit())
            .take(self.recursive_tree.elems.len())
            .collect::<Vec<_>>();

        for (idx, node) in self.recursive_tree.elems.iter().enumerate().rev() {
            let alg_res = {
                // each node is only referenced once so just remove it, also we know it's there so unsafe is fine
                let node = node.map_layer(|ArenaIndex(x)| unsafe {
                    let maybe_uninit = std::mem::replace(
                        results.get_unchecked_mut(x - self.offset),
                        MaybeUninit::uninit(),
                    );
                    maybe_uninit.assume_init()
                });
                collapse_layer(node)
            };
            results[idx].write(alg_res);
        }

        unsafe {
            let maybe_uninit = std::mem::replace(
                results.get_unchecked_mut(ArenaIndex::head().0),
                MaybeUninit::uninit(),
            );
            maybe_uninit.assume_init()
        }
    }
}

impl<'a, A: 'a, Cached, Wrapped: 'a, U> CollapseWithContext<'a, A, Wrapped>
    for RecursiveTreeRefWithOffsetAndContext<'a, U, Cached>
where
    &'a U: MapLayer<(&'a Cached, &'a A), To = Wrapped, Unwrapped = ArenaIndex>,
{
    // TODO: starting with low-perf option vec for correctness
    fn collapse_layers_3<F: FnMut(Wrapped) -> &'a A>(&self, mut collapse_layer: F) -> &'a A {
        let mut results: Vec<Option<&'a A>> = std::iter::repeat_with(|| None)
            .take(self.recursive_tree.elems.len())
            .collect::<Vec<_>>();

        for (idx, node) in self.recursive_tree.elems.iter().enumerate().rev() {
            let alg_res: &'a A = {
                let node = node.map_layer(|ArenaIndex(x)| {
                    let res: &'a A = results.get(x - self.offset).unwrap().unwrap();

                    let cached: &'a Cached =
                        &self.context.get(x - self.offset).unwrap().as_ref().unwrap();

                    (cached, res)
                });
                collapse_layer(node)
            };
            results[idx - self.offset] = Some(alg_res);
        }

        results.get(ArenaIndex::head().0).unwrap().unwrap()
    }
}
