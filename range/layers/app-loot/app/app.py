from flask import Flask
app = Flask(__name__)

@app.route("/")
def index():
    return """<html><body>
<h2>Internal Loot Server</h2>
<p>flag{FLAG_PLACEHOLDER}</p>
<p>DB credentials: postgres://admin:DB_PASS_PLACEHOLDER@10.99.3.20/prod</p>
</body></html>"""

if __name__ == "__main__":
    app.run(host="0.0.0.0", port=5000)
