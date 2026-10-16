#![deny(unsafe_op_in_unsafe_fn)]

use crate::av1::cpu::c_arc::CArc;
use crate::av1::cpu::c_box::CRef;
use crate::av1::cpu::error::Rav1dResult;
use crate::av1::cpu::include::dav1d::common::Rav1dDataProps;
use crate::av1::cpu::include::dav1d::data::Rav1dData;

impl From<CArc<[u8]>> for Rav1dData {
    fn from(data: CArc<[u8]>) -> Self {
        let size = data.len();
        Self {
            data: Some(data),
            m: Rav1dDataProps {
                size,
                ..Default::default()
            },
        }
    }
}

impl Rav1dData {
    pub fn wrap(data: CRef<[u8]>) -> Rav1dResult<Self> {
        Ok(CArc::wrap(data)?.into())
    }
}

impl AsRef<[u8]> for Rav1dData {
    fn as_ref(&self) -> &[u8] {
        match &self.data {
            Some(data) => data.as_ref(),
            None => &[],
        }
    }
}
