//! M4 (Stream Deck): the station's `state` lists the carts that are playing, so a Fire Item key can light while
//! its item plays. Real station process, real HTTP, as in M1.7.

mod support_api;

use serde_json::json;
use support_api::*;

#[test]
fn state_lists_the_carts_that_play_and_drops_them_when_they_end() {
    let mut e = Env::new(
        "m4-carts",
        &Opts {
            tones: 3,
            secs: 4.0,
            ..Opts::default()
        },
    );
    e.spawn_station(&[]);
    let st = e.st();
    let cart_item = e.items[2];

    let r = wait_for("the first state", 15.0, || {
        let s = get(&st, "/api/v1/state").json();
        s["carts"].is_array().then_some(s)
    });
    assert_eq!(r["carts"], json!([]), "no carts at the start: {r}");

    post(&st, "/api/v1/transport/play", &json!({}));
    wait_for("the station to play", 15.0, || {
        (get(&st, "/api/v1/state").json()["state"] == "playing").then_some(())
    });

    let r = post(&st, "/api/v1/cart", &json!({"item_id": cart_item}));
    assert_eq!(r.status, 202, "{}", r.json());
    let carts = wait_for("the cart in the state", 6.0, || {
        let c = get(&st, "/api/v1/state").json()["carts"].clone();
        let playing = c.as_array().is_some_and(|a| !a.is_empty());
        playing.then_some(c)
    });
    assert_eq!(carts[0]["item_id"], cart_item);
    assert!(carts[0]["started_at"]
        .as_str()
        .is_some_and(|s| s.contains('T')));

    // The cart is not the on-air item.
    let state = get(&st, "/api/v1/state").json();
    assert_ne!(
        state["now"]["item_id"],
        json!(cart_item),
        "a cart is not `now`: {state}"
    );

    wait_for("the cart to leave the state", 12.0, || {
        get(&st, "/api/v1/state").json()["carts"]
            .as_array()
            .is_some_and(|a| a.is_empty())
            .then_some(())
    });
}
