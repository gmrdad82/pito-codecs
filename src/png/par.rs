use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, PoisonError};

const MAX_THREADS: usize = 16;

static LIMIT: AtomicUsize = AtomicUsize::new(0);

#[doc(hidden)]
pub fn limit_threads(n: usize) {
    LIMIT.store(n, Ordering::Relaxed);
}

pub fn workers(jobs: usize) -> usize {
    let cap = match LIMIT.load(Ordering::Relaxed) {
        0 => MAX_THREADS,
        n => n,
    };
    std::thread::available_parallelism()
        .map_or(1, |n| n.get())
        .min(jobs)
        .min(cap)
        .max(1)
}

pub fn map<S, T: Send>(
    jobs: usize,
    threads: usize,
    state: impl Fn() -> S + Sync,
    work: impl Fn(&mut S, usize) -> T + Sync,
) -> Vec<T> {
    if threads <= 1 || jobs <= 1 {
        let mut s = state();
        return (0..jobs).map(|i| work(&mut s, i)).collect();
    }
    let next = AtomicUsize::new(0);
    let done = Mutex::new(Vec::with_capacity(jobs));
    std::thread::scope(|scope| {
        for _ in 0..threads {
            scope.spawn(|| {
                let mut s = state();
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    if i >= jobs {
                        break;
                    }
                    let v = work(&mut s, i);
                    done.lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .push((i, v));
                }
            });
        }
    });
    let mut v = done.into_inner().unwrap_or_else(PoisonError::into_inner);
    v.sort_by_key(|x| x.0);
    v.into_iter().map(|x| x.1).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn results_come_back_in_job_order_at_any_thread_count() {
        for threads in [1, 2, 5, 16] {
            let got = map(
                40,
                threads,
                || 0u32,
                |s, i| {
                    *s += 1;
                    i * i
                },
            );
            assert_eq!(got, (0..40).map(|i| i * i).collect::<Vec<_>>());
        }
    }

    #[test]
    fn no_jobs_is_no_results() {
        assert!(map(0, 4, || (), |_, i| i).is_empty());
    }

    #[test]
    fn workers_never_exceed_the_jobs() {
        assert_eq!(workers(1), 1);
        assert!(workers(1000) >= 1 && workers(1000) <= MAX_THREADS);
    }
}
