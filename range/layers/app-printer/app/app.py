from flask import Flask
app = Flask(__name__)

@app.route("/")
def index():
    return "Printer Management Interface - Status: Ready"

@app.route("/jobs")
def jobs():
    return "Active print jobs: 0"

if __name__ == "__main__":
    app.run(host="0.0.0.0", port=5000)
