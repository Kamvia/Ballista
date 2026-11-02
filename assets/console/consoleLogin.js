// File: ballista/assets/console/consoleLogin.js
(function () {
  "use strict";

  var form = document.getElementById("signin-form");
  var input = document.getElementById("token");
  var error = document.getElementById("signin-error");
  var button = document.getElementById("signin-btn");

  function show(msg) {
    error.textContent = msg;
    error.hidden = !msg;
  }

  form.addEventListener("submit", function (ev) {
    ev.preventDefault();
    var token = input.value.trim();
    if (token.length < 32 || token.length > 512 || /\s/.test(token)) {
      show("Paste the full API token. It is at least 32 characters and has no spaces.");
      input.focus();
      return;
    }
    button.disabled = true;
    show("");
    fetch("/console/api/login", {
      method: "POST",
      credentials: "same-origin",
      cache: "no-store",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ token: token })
    }).then(function (res) {
      if (res.ok) {
        input.value = "";
        window.location.replace("/console/");
        return;
      }
      if (res.status === 429) {
        show("Too many failed attempts from this address. Wait 30 seconds and try again.");
      } else if (res.status === 401) {
        show("This token is not valid on this server.");
      } else {
        show("Sign-in failed (" + res.status + ").");
      }
      button.disabled = false;
      input.select();
    }).catch(function () {
      show("The server could not be reached.");
      button.disabled = false;
    });
  });
}());
