//! OA 翻页：默认单页 100，超出按 `next_cursor` 拉取；最多 50 页（防无限循环）。

use super::client::OaClient;
use super::error::OaPullError;
use super::models::{from_dtos, OaEventDto, OaPullRequest, OaPullResponse};
use super::retry::fetch_page_with_retry;

use crate::schemas::OaEvent;

pub const DEFAULT_PAGE_SIZE: u32 = 100;
pub const MAX_PAGES: u32 = 50;

/// 把单页 DTO 列表 + 当前 cursor 推到累加器；返回是否还有下一页。
pub fn take_page(resp: &mut OaPullResponse, cursor: &mut Option<String>) -> Vec<OaEventDto> {
    let events = std::mem::take(&mut resp.events);
    *cursor = resp.next_cursor.take();
    events
}

/// 翻页循环：自动按 `next_cursor` 拉取直到 `is_last_page` 或达 50 页上限。
pub async fn fetch_events_paginated(
    client: &OaClient,
    week_id: &str,
) -> Result<Vec<OaEvent>, OaPullError> {
    let mut all: Vec<OaEventDto> = Vec::new();
    let mut cursor: Option<String> = None;
    let mut page_count = 0u32;
    loop {
        if page_count >= MAX_PAGES {
            return Err(OaPullError::Pagination(format!(
                "翻页超过 {MAX_PAGES} 页上限"
            )));
        }
        let req = match &cursor {
            Some(c) => OaPullRequest::next(week_id, c.clone(), DEFAULT_PAGE_SIZE),
            None => OaPullRequest::first(week_id),
        };
        let resp = fetch_page_with_retry(client, &req).await?;
        let page_cursor = resp.next_cursor.clone();
        all.extend_from_page(resp.events.clone());
        page_count += 1;
        if page_cursor.is_none() || resp.is_last_page() {
            break;
        }
        cursor = page_cursor;
    }
    from_dtos(&all).map_err(OaPullError::SchemaMissing)
}

trait VecExt {
    fn extend_from_page(&mut self, page: Vec<OaEventDto>);
}
impl VecExt for Vec<OaEventDto> {
    fn extend_from_page(&mut self, page: Vec<OaEventDto>) {
        self.extend(page);
    }
}

/// 单页拉取 + 翻页的辅助函数（测试用）：直接走 retry 通道。
pub async fn fetch_one_page(
    client: &OaClient,
    req: &OaPullRequest,
) -> Result<OaPullResponse, OaPullError> {
    fetch_page_with_retry(client, req).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn take_page_consumes_events_and_cursor() {
        let mut resp = OaPullResponse {
            events: vec![OaEventDto {
                oa_event_id: "1".into(),
                resolution_id: "r".into(),
                action: "started".into(),
                actor_id: "u".into(),
                event_time: "2026-10-12T09:00:00Z".into(),
            }],
            next_cursor: Some("cur1".into()),
        };
        let mut cursor = None;
        let taken = take_page(&mut resp, &mut cursor);
        assert_eq!(taken.len(), 1);
        assert_eq!(cursor.as_deref(), Some("cur1"));
        assert!(resp.events.is_empty());
        assert!(resp.next_cursor.is_none());
    }
}