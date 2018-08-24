use std::marker::PhantomData;
use std::sync::Arc;
use std::any::Any;

use task::{DynFn,DynFn2,BASS};
use graph::*;
use scheduler::Scheduler;

struct Lift<A>(A);

impl <A: Any + Send + Sync + Clone> Input for Lift<A> {
    fn read(&self) -> BASS {
        Box::new(self.0.clone())
    }
}

#[derive(Clone)]
pub struct Deferred<A> {
    graph: Arc<Graph>,
    items: PhantomData<A>
}

impl <A: Any + Send + Sync> Deferred<A> {
    
    pub fn apply<B: Any + Send + Sync, F: Send + Sync + 'static + Fn(&A) -> B>(&self, f: F) -> Deferred<B> {
        let ng = Graph::create_task(
            FnArgs::Single(self.graph.clone()), DynFn::new(f), "Apply");
        Deferred {
            graph: ng,
            items: PhantomData
        }

    }

    pub fn join<B: Any + Send + Sync, C: Any + Send + Sync, F: Send + Sync + 'static + Fn(&A, &B) -> C>(&self, other: &Deferred<B>, f: F) -> Deferred<C> {
        let ng = Graph::create_task(
            FnArgs::Join(self.graph.clone(), other.graph.clone()), 
            DynFn2::new(f), "Join");

        Deferred {
            graph: ng,
            items: PhantomData
        }

    }
}

impl <A: Any + Send + Sync + Clone> Deferred<A> {
    pub fn lift(a: A, name: Option<&str>) -> Self {
        let graph = Graph::create_input(Lift(a), name.unwrap_or("Input"));
        Deferred {
            graph: graph,
            items: PhantomData
        }
    }

    pub fn run<S: Scheduler>(&self, s: &mut S) -> Option<A> {
        s.compute(self.graph.clone()).and_then(|v| { 
            Arc::try_unwrap(v).ok().and_then(|ab| {
                ab.downcast_ref::<A>().map(|x| x.clone())
            })
        })
    }
}

pub fn batch_apply<
    A: Any + Send + Sync + Clone, 
    B: Any + Send + Sync, 
    F: 'static + Sync + Send + Clone + Fn(usize, &A) -> B
    >(defs: &[Deferred<A>], f: F) 
-> Vec<Deferred<B>> {
    let mut nps = Vec::with_capacity(defs.len());
    let fa = Arc::new(f);
    for (idx, p) in defs.iter().enumerate() {
        let mf = fa.clone();
        let np = p.apply(move |vs| { mf(idx, vs) }); 
        nps.push(np);
    }   
    nps 
}

pub fn tree_reduce<A: Any + Send + Sync + Clone, 
                   F: 'static + Sync + Send + Clone + Fn(&A, &A) -> A
>(
    defs: &[Deferred<A>], 
    f: F
) -> Option<Deferred<A>> {
    tree_reduce_until(defs, 1, f).map(|mut defs| {
        defs.remove(0)
    })
}

pub fn tree_reduce_until<A: Any + Send + Sync + Clone, 
                   F: 'static + Sync + Send + Clone + Fn(&A, &A) -> A
>(
    defs: &[Deferred<A>], 
    parts: usize, 
    f: F
) -> Option<Vec<Deferred<A>>> {
    if defs.len() == 0 {
        None
    } else if defs.len() <= parts {
        Some(defs.clone().to_vec())
    } else {
        // First pass
        let mut pass = Vec::new();
        for i in (0..defs.len() - 1).step_by(2) {
            pass.push(defs[i].join(&defs[i+1], f.clone()));
        }
        if defs.len() % 2 == 1 {
            pass.push(defs[defs.len() - 1].clone());
        }
        tree_reduce_until(&pass, parts, f)
    }
}

#[cfg(test)]
mod def_test {
    use super::*;
    use scheduler::{LeveledScheduler,GreedyScheduler};

    #[test]
    fn test_tree_reduce() {
        let v: Vec<_> = (0..999usize).into_iter()
            .map(|x| Deferred::lift(x, None))
            .collect();

        let res = (0..999usize).sum();

        let agg = tree_reduce(&v, |x, y| x + y).unwrap();
        let results = agg.run(&mut LeveledScheduler);
        assert_eq!(results, Some(res));
    }

    #[test]
    fn test_tree_reduce_greedy() {
        let v: Vec<_> = (0..2usize).into_iter()
            .map(|x| Deferred::lift(x, None))
            .collect();

        let res = (0..2usize).sum();

        let agg = tree_reduce(&v, |x, y| x + y).unwrap();
        let results = agg.run(&mut GreedyScheduler::new(1));
        assert_eq!(results, Some(res));
    }

}
