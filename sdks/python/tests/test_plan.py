# File: ballista/sdks/python/tests/test_plan.py
import json

import pytest
from pydantic import ValidationError

from ballista_client import Feeder, Load, Plan, Scenario, Target, Threshold, Wait
from ballista_client.plan import batch, feed, get, post, think, transaction

SHAPE = "fn shape(t) { if t > 10.0 { () } else { 5.0 } }"


def shop_plan(base_url: str = "http://127.0.0.1:8080/") -> Plan:
    return Plan(
        name="shop api",
        target=Target(base_url=base_url, headers={"accept": "application/json"}),
        load=Load.users(("2s", 4), ("4s", 4)),
        scenarios=[
            Scenario(
                name="buyer",
                wait=Wait.uniform("50ms", "100ms"),
                on_start=[feed("users"), post("/login", json={"username": "${users.username}", "password": "${users.password}"}, capture={"token": "json.token"})],
                steps=[
                    get("/items", name="list items", capture={"item_id": "json.items.1.id"}),
                    think("20ms"),
                    *transaction(
                        "purchase",
                        get("/items/${item_id}", name="item", contains="stock"),
                        post("/cart", name="cart", headers={"authorization": "Bearer ${token}"}, json={"item": "${item_id}", "qty": 1}, expect=[201, 503]),
                    ),
                    batch(get("/items"), get("/slow", name="slow page"), name="page", concurrency=2),
                ],
            ),
        ],
        feeders=[Feeder(name="users", rows=[{"username": f"user{i}", "password": "secret-pass"} for i in range(1, 6)])],
        thresholds=[Threshold(metric="p95", request="purchase", max=1000)],
        percentiles=[75, 99.9],
    )


def test_document_structure_is_json_ready() -> None:
    doc = shop_plan().to_document()
    json.dumps(doc)
    assert doc["load"] == {"mode": "users", "stages": [{"duration": "2s", "target": 4.0}, {"duration": "4s", "target": 4.0}]}
    sc = doc["scenario"][0]
    assert sc["wait"] == {"between": ["50ms", "100ms"]}
    assert sc["on_start"][1]["json"]["username"] == "${users.username}"
    assert [s.get("txn_begin") for s in sc["step"]][2] == "purchase"
    assert sc["step"][-1]["batch"][1] == {"get": "/slow", "name": "slow page"}
    assert doc["feeder"][0]["rows"][0] == {"username": "user1", "password": "secret-pass"}
    assert doc["threshold"][0] == {"metric": "p95", "request": "purchase", "max": 1000.0}
    assert doc["report"] == {"percentiles": [75.0, 99.9]}


def test_inline_script_and_shape() -> None:
    p = Plan(
        name="x",
        target=Target(base_url="http://127.0.0.1/"),
        load=Load.shape(SHAPE),
        scenarios=[Scenario(name="s", source="fn run(step, res) { () }")],
    )
    doc = p.to_document()
    assert doc["load"] == {"mode": "users", "shape_source": SHAPE}
    assert doc["scenario"][0] == {"name": "s", "source": "fn run(step, res) { () }"}


def test_rejects_invalid_specs() -> None:
    with pytest.raises(ValidationError):
        Target(base_url="ftp://x")
    with pytest.raises(ValidationError):
        Scenario(name="bad name!", steps=[get("/")])
    with pytest.raises(ValidationError):
        Scenario(name="a", source="x", steps=[get("/")])
    with pytest.raises(ValidationError):
        get("/", capture={"not-ident": "json.a"})
    with pytest.raises(ValidationError):
        Threshold(metric="p95")
    with pytest.raises(ValidationError):
        think("soon")
    with pytest.raises(ValidationError):
        Feeder(name="u", rows=[{"a": 1}], path="data.csv")
    with pytest.raises(ValidationError):
        Load(mode="users")
