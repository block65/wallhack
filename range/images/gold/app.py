from flask import Flask

app = Flask(__name__)


@app.route("/")
def index():
    return "CONGRATULATIONS! flag{y0u_f0und_th3_g0ld}"


if __name__ == "__main__":
    app.run(host="0.0.0.0", port=5000)
