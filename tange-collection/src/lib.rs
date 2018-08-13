extern crate tange;

pub mod utils;
mod partition;

use std::fs;
use std::any::Any;
use std::io::prelude::*;
use std::io::BufWriter;
use std::hash::Hash;

use tange::deferred::{Deferred, batch_apply, tree_reduce};
use tange::scheduler::Scheduler;
use self::partition::*;


#[derive(Clone)]
pub struct Collection<A>  {
    partitions: Vec<Deferred<Vec<A>>>
}

impl <A: Any + Send + Sync + Clone> Collection<A> {
    pub fn from_vec(vs: Vec<A>) -> Collection<A> {
        Collection {
            partitions: vec![Deferred::lift(vs, None)],
        }
    }

    pub fn n_partitions(&self) -> usize {
        self.partitions.len()
    }

    pub fn concat(&self, other: &Collection<A>) -> Collection<A> {
        let mut nps: Vec<Deferred<Vec<A>>> = self.partitions.iter()
            .map(|p| (*p).clone()).collect();
        for p in other.partitions.iter() {
            nps.push(p.clone());
        }
        Collection { partitions: nps }
    }
    
    pub fn map<B: Any + Send + Sync + Clone, F: 'static + Sync + Send + Clone + Fn(&A) -> B>(&self, f: F) -> Collection<B> {
        let out = batch_apply(&self.partitions, move |_idx, vs| {
            let mut agg = Vec::with_capacity(vs.len());
            for v in vs {
                agg.push(f(v));
            }
            agg
        });
        Collection { partitions: out }
    }

    pub fn filter<F: 'static + Sync + Send + Clone + Fn(&A) -> bool>(&self, f: F) -> Collection<A> {
        let out = batch_apply(&self.partitions, move |_idx, vs| {
            let mut agg = Vec::with_capacity(vs.len());
            for v in vs {
                if f(v) {
                    agg.push(v.clone());
                }
            }
            agg
        });
        Collection { partitions: out }
    }
    
    pub fn split(&self, n_chunks: usize) -> Collection<A> {
        self.partition(n_chunks, |idx, _k| idx)
    }

    pub fn partition<F: 'static + Sync + Send + Clone + Fn(usize, &A) -> usize>(&self, partitions: usize, f: F) -> Collection<A> {
        let new_chunks = partition(&self.partitions, 
                                   partitions, 
                                   |v| Box::new(v.clone().into_iter()),
                                   f);
        // Loop over each bucket
        Collection { partitions: new_chunks }
    }

    pub fn fold_by<K: Any + Sync + Send + Clone + Hash + Eq,
                   B: Any + Sync + Send + Clone,
                   D: 'static + Sync + Send + Clone + Fn() -> B, 
                   F: 'static + Sync + Send + Clone + Fn(&A) -> K, 
                   O: 'static + Sync + Send + Clone + Fn(&B, &A) -> B,
                   R: 'static + Sync + Send + Clone + Fn(&B, &B) -> B>(
        &self, key: F, default: D, binop: O, reduce: R, partitions: usize
    ) -> Collection<(K,B)> {
        let results = fold_by(&self.partitions, key, default, binop, reduce, partitions);
        Collection { partitions: results }
    }

                   
    pub fn partition_by_key<
        K: Any + Sync + Send + Clone + Hash + Eq,
        F: 'static + Sync + Send + Clone + Fn(&A) -> K
    >(&self, n_chunks: usize, key: F) -> Collection<A> {
        let results = partition_by_key(&self.partitions, n_chunks, key);
        let groups = results.into_iter().map(|part| concat(&part)).collect();
        Collection {partitions: groups}
    }

    pub fn sort_by<
        K: Ord,
        F: 'static + Sync + Send + Clone + Fn(&A) -> K
    >(&self, key: F) -> Collection<A> {
        let nps = batch_apply(&self.partitions, move |_idx, vs| {
            let mut v2: Vec<_> = vs.clone();
            v2.sort_by_key(|v| key(v));
            v2
        });
        Collection { partitions: nps }
    }


    pub fn run<S: Scheduler>(&self, s: &mut S) -> Option<Vec<A>> {
        let cat = tree_reduce(&self.partitions, |x, y| {
            let mut v1: Vec<_> = (*x).clone();
            for yi in y {
                v1.push(yi.clone());
            }
            v1
        });
        cat.and_then(|x| x.run(s))
    }
}

impl <A: Any + Send + Sync + Clone> Collection<Vec<A>> {
    pub fn flatten(&self) -> Collection<A> {
        let nps = batch_apply(&self.partitions, |_idx, vss| {
            let mut new_v = Vec::new();
            for vs in vss {
                for v in vs {
                    new_v.push(v.clone());
                }
            }
            new_v
        });

        Collection { partitions: nps }
    }
}

impl <A: Any + Send + Sync + Clone> Collection<A> {
    pub fn count(&self) -> Collection<usize> {
        let nps = batch_apply(&self.partitions, |_idx, vs| vs.len());
        let count = tree_reduce(&nps, |x, y| x + y).unwrap();
        let out = count.apply(|x| vec![*x]);
        Collection { partitions: vec![out] }
    }
}

impl <A: Any + Send + Sync + Clone + PartialEq + Hash + Eq> Collection<A> {
    pub fn frequencies(&self, partitions: usize) -> Collection<(A, usize)> {
        //self.partition(chunks, |x| x);
        self.fold_by(|s| s.clone(), 
                     || 0usize, 
                     |acc, _l| *acc + 1, 
                     |x, y| *x + *y, 
                     partitions)
    }
}

// Writes out data
impl Collection<String> {
    pub fn sink(&self, path: &'static str) -> Collection<usize> {
        let pats = batch_apply(&self.partitions, move |idx, vs| {
            fs::create_dir_all(path)
                .expect("Welp, something went terribly wrong when creating directory");

            let file = fs::File::create(&format!("{}/{}", path, idx))
                .expect("Issues opening file!");
            let mut bw = BufWriter::new(file);

            let size = vs.len();
            for line in vs {
                bw.write(line.as_bytes()).expect("Error writing out line");
                bw.write(b"\n").expect("Error writing out line");
            }

            vec![size]
        });
        
        Collection { partitions: pats }
    }
}

#[cfg(test)]
mod test_lib {
    use super::*;
    use tange::scheduler::LeveledScheduler;

    #[test]
    fn test_fold_by() {
        let col = Collection::from_vec(vec![1,2,3,1,2usize]);
        let out = col.fold_by(|x| *x, || 0, |x, _y| x + 1, |x, y| x + y, 1);
        let mut results = out.run(&mut LeveledScheduler).unwrap();
        results.sort();
        assert_eq!(results, vec![(1, 2), (2, 2), (3, 1)]);
    }

    #[test]
    fn test_fold_by_parts() {
        let col = Collection::from_vec(vec![1,2,3,1,2usize]);
        let out = col.fold_by(|x| *x, || 0, |x, _y| x + 1, |x, y| x + y, 2);
        assert_eq!(out.partitions.len(), 2);
        let mut results = out.run(&mut LeveledScheduler).unwrap();
        results.sort();
        assert_eq!(results, vec![(1, 2), (2, 2), (3, 1)]);
    }

    #[test]
    fn test_partition_by_key() {
        let col = Collection::from_vec(vec![1,2,3,1,2usize]);
        let computed = col.partition_by_key(2, |x| *x)
            .sort_by(|x| *x);
        assert_eq!(computed.partitions.len(), 2);
        let results = computed.run(&mut LeveledScheduler).unwrap();
        assert_eq!(results, vec![2, 2, 3, 1, 1]);
    }

    #[test]
    fn test_partition() {
        let col = Collection::from_vec(vec![1,2,3,1,2usize]);
        let computed = col.partition(2, |_idx, x| x % 2)
            .sort_by(|x| *x);
        assert_eq!(computed.partitions.len(), 2);
        let results = computed.run(&mut LeveledScheduler).unwrap();
        assert_eq!(results, vec![2, 2, 1, 1, 3]);
    }

}
