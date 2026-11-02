# File: ballista/benchmarks/locustfile.py
from locust import HttpUser, constant, task
from locust.contrib.fasthttp import FastHttpUser


class Reader(HttpUser):
    wait_time = constant(1)

    @task
    def item(self) -> None:
        self.client.get("/items/3", name="item")


class FastReader(FastHttpUser):
    wait_time = constant(1)

    @task
    def item(self) -> None:
        self.client.get("/items/3", name="item")
