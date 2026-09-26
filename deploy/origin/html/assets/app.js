// Sample origin script. Real customers would put anything here; auditors only care that bytes match the origin.
document.addEventListener("DOMContentLoaded", function () {
  document.body.dataset.origin = "acme-devnet";
});
