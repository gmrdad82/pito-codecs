use crate::av1::cpu::sync::Mutex;

pub struct MemPool<T> {
    bufs: Mutex<Vec<Vec<T>>>,
}

impl<T> MemPool<T> {
    pub const fn new() -> Self {
        Self {
            bufs: Mutex::new(Vec::new()),
        }
    }

    pub fn _pop(&self, size: usize) -> Vec<T> {
        if let Some(mut buf) = self.bufs.lock().pop() {
            if size > buf.capacity() {
                buf.reserve(size - buf.len());
            }
            return buf;
        }

        Vec::with_capacity(size)
    }

    pub fn pop_init(&self, size: usize, init_value: T) -> Vec<T>
    where
        T: Copy,
    {
        if let Some(buf) = self.bufs.lock().pop() {
            if size <= buf.len() {
                return buf;
            }
        }

        vec![init_value; size]
    }

    pub fn push(&self, buf: Vec<T>) {
        self.bufs.lock().push(buf);
    }
}

impl<T> Default for MemPool<T> {
    fn default() -> Self {
        Self::new()
    }
}
