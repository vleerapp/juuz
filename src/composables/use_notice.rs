use crate::server::utils::notice::Notice;
use topcoat::Result;
use topcoat::context::Cx;
use topcoat::router::query_params;

#[query_params(error = bad_request)]
pub struct Query {
    pub n: Option<String>,
    pub range: Option<String>,
    pub scope: Option<String>,
}

pub fn notice_of(cx: &Cx) -> Result<Option<Notice>> {
    let query = query_params::<Query>(cx)?;
    Ok(Notice::parse(query.n.as_deref()))
}
