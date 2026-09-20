import asyncio
import json

import websockets

URI = "ws://localhost:8080/ws"


async def receive_msg(ws, name):
    """Вспомогательная функция для чтения и вывода сообщений"""
    try:
        res = await ws.recv()
        data = json.loads(res)
        print(f"[{name} Получил]: {data}")
        return data
    except websockets.exceptions.ConnectionClosed:
        print(f"[{name}]: Соединение закрыто")
        return None


async def run_test():
    print("=== Запуск интеграционного теста чата ===")

    # 1. Подключаем Alice
    async with websockets.connect(URI) as ws_alice:
        print("\n--- Авторизация Alice ---")
        await ws_alice.send(
            json.dumps(
                {"type": "auth", "username": "alice", "password": "alice_password"}
            )
        )

        # Ожидаем auth_ok и peers от сервера
        await receive_msg(ws_alice, "Alice")
        await receive_msg(ws_alice, "Alice")

        # 2. Подключаем Bob в отдельном соединении
        async with websockets.connect(URI) as ws_bob:
            print("\n--- Авторизация Bob ---")
            await ws_bob.send(
                json.dumps(
                    {"type": "auth", "username": "bob", "password": "bob_password"}
                )
            )
            await receive_msg(ws_bob, "Bob")
            await receive_msg(ws_bob, "Bob")

            # 3. Alice отправляет сообщение Бобу
            print("\n--- Отправка сообщения: Alice -> Bob ---")
            await ws_alice.send(
                json.dumps(
                    {"type": "send", "to": "bob", "payload": "Привет, Боб! Как дела?"}
                )
            )

            # Bob должен мгновенно получить сообщение в реальном времени
            msg_for_bob = await receive_msg(ws_bob, "Bob")

            # 4. Alice запрашивает у Боба историю сообщений
            print("\n--- Запрос истории: Alice -> Bob ---")
            await ws_alice.send(
                json.dumps({"type": "history_request", "from": "bob", "since": 0})
            )

            # Сервер перенаправляет запрос истории Бобу
            req_for_bob = await receive_msg(ws_bob, "Bob")

            if req_for_bob and req_for_bob.get("type") == "history_request":
                # Боб отвечает на запрос истории (возвращает массив StoredMsg)
                print("\n--- Ответ на запрос истории: Bob -> Alice ---")
                await ws_bob.send(
                    json.dumps(
                        {
                            "type": "history_response",
                            "to": "alice",
                            "messages": [
                                {
                                    "from": "alice",
                                    "to": "bob",
                                    "ts": 1600000000,
                                    "payload": "Привет, Боб! Как дела?",
                                }
                            ],
                        }
                    )
                )

                # Alice получает историю от Боба через сервер
                await receive_msg(ws_alice, "Alice")

    print("\n=== Тест успешно завершен ===")


if __name__ == "__main__":
    asyncio.run(run_test())
