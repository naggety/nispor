use rtnetlink;

#[derive(Debug, Clone)]
pub enum ErrorKind {
    NetlinkError,
    NisporBug,
}

#[derive(Debug, Clone)]
pub struct NisporError {
    pub kind: ErrorKind,
    pub msg: String,
}

impl std::fmt::Display for NisporError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.msg)
    }
}

impl std::error::Error for NisporError {
    /* TODO */
}

impl std::convert::From<rtnetlink::Error> for NisporError {
    fn from(e: rtnetlink::Error) -> Self {
        NisporError {
            kind: ErrorKind::NetlinkError,
            msg: e.to_string(),
        }
    }
}

impl std::convert::From<std::io::Error> for NisporError {
    fn from(e: std::io::Error) -> Self {
        NisporError {
            kind: ErrorKind::NisporBug,
            msg: e.to_string(),
        }
    }
}
