//! A disabled line (user bug 108) is not a router line: it gets no request,
//! even when every enabled line fails.

mod common;

use common::*;
use superba_core::catalog::{
    KRILL_LINE_CDN, KRILL_LINE_CN_FAST, KRILL_LINE_OVERSEAS, builtin_profiles, krill_site,
};
use superba_core::{LineId, SiteKind};
use superba_router::members::{MemberPath, member_from_site};

const OK: &str = r#"{"ok":true}"#;
const BODY: &str = r#"{"model":"gpt-x","input":"hello"}"#;

#[tokio::test]
async fn a_disabled_line_gets_no_request_even_when_the_others_fail() {
    let busy = upstream(Mode::Json(503, "{}")).await;
    let healthy = upstream(Mode::Json(200, OK)).await;
    let mut site = krill_site();
    site.kind = SiteKind::Compatible;
    for line in &mut site.lines {
        line.origin = if line.id.as_str() == KRILL_LINE_CDN {
            superba_core::Origin::parse(healthy.base("/").as_str()).unwrap()
        } else {
            superba_core::Origin::parse(busy.base("/").as_str()).unwrap()
        };
    }
    let mut profile = builtin_profiles()[1].clone();
    profile.builtin = None;
    profile.site_id = site.id.clone();
    profile.set_line_priority([
        LineId::new(KRILL_LINE_CDN),
        LineId::new(KRILL_LINE_OVERSEAS),
    ]);
    profile
        .set_line_disabled(&site, &LineId::new(KRILL_LINE_CDN), true)
        .unwrap();
    let order = profile.enabled_line_ids(&site);
    assert_eq!(
        order,
        [
            LineId::new(KRILL_LINE_OVERSEAS),
            LineId::new(KRILL_LINE_CN_FAST)
        ]
    );
    let member = member_from_site(&site, &order, &MemberPath::Custom("/v1".into())).unwrap();
    assert!(member.lines.iter().all(|l| l.id != KRILL_LINE_CDN));
    let r = start(test_config(vec![member])).await;
    for _ in 0..3 {
        let resp = r.post(Some("s"), BODY).await;
        assert_eq!(resp.status(), 502, "no fallback to the disabled line");
        let v: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(v["error"]["type"], "router_unavailable");
    }
    assert!(busy.hits() >= 2, "the enabled lines were tried");
    assert_eq!(healthy.hits(), 0, "the disabled line got no request");
    let st = r.status().await;
    assert!(
        st.members[0].lines.iter().all(|l| l.id != KRILL_LINE_CDN),
        "the router does not know the disabled line"
    );
}

#[test]
fn the_last_enabled_line_cannot_be_disabled() {
    let site = krill_site();
    let mut profile = builtin_profiles()[1].clone();
    profile.builtin = None;
    profile.site_id = site.id.clone();
    profile
        .set_line_disabled(&site, &LineId::new(KRILL_LINE_CDN), true)
        .unwrap();
    profile
        .set_line_disabled(&site, &LineId::new(KRILL_LINE_OVERSEAS), true)
        .unwrap();
    assert!(
        profile
            .set_line_disabled(&site, &LineId::new(KRILL_LINE_CN_FAST), true)
            .is_err()
    );
    assert_eq!(
        profile.enabled_line_ids(&site),
        [LineId::new(KRILL_LINE_CN_FAST)]
    );
}
