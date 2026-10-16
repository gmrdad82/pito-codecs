macro_rules! const_for {
    ($index:ident in $range:expr, step_by $step:expr => $block:block) => {{
        use std::ops::Range;

        let range: Range<_> = $range;
        let step = $step;
        let mut $index = range.start;
        while $index < range.end {
            $block
            $index += step;
        }
    }};
    ($index:ident in $range:expr => $block:block) => {
        const_for!($index in $range, step_by 1 => $block)
    };
}

pub(crate) use const_for;

macro_rules! const_min {
    ($a:expr, $b:expr) => {{
        let a = $a;
        let b = $b;
        if a < b { a } else { b }
    }};
}

pub(crate) use const_min;
