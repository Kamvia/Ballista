// File: ballista/assets/login.js
(function () {
  "use strict";

  var form = document.getElementById("login-form");
  var input = document.getElementById("password");
  var btn = document.getElementById("login-btn");
  var errorEl = document.getElementById("login-error");

  function showError(msg) {
    errorEl.textContent = msg;
    errorEl.hidden = false;
  }

  form.addEventListener("submit", function (ev) {
    ev.preventDefault();
    errorEl.hidden = true;
    var password = input.value;
    if (!password || password.length > 1024) {
      showError("Enter a valid password.");
      return;
    }
    btn.disabled = true;
    fetch("/api/login", {
      method: "POST",
      credentials: "same-origin",
      cache: "no-store",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ password: password })
    })
      .then(function (res) {
        if (res.ok) {
          input.value = "";
          window.location.replace("/");
          return;
        }
        if (res.status === 429) {
          showError("Too many attempts. Wait a minute and try again.");
        } else {
          showError("Invalid credentials.");
        }
      })
      .catch(function () {
        showError("Network error.");
      })
      .finally(function () {
        btn.disabled = false;
      });
  });
})();
