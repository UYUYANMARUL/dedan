// Fetch token of the mock node (ARCHITECTURE.md §7), the same string the Pingora node mints:
//   X-Dedan-Node: <label>.<minute>.<hex(HMAC-SHA256(k_hour, "<minute>|<client /24>"))>
// The devnet's k_hour is one static key (deploy/devnet/keys.json) that the node's hot key reveals on chain,
// so tokens verify exactly as production ones do.
var crypto = require('crypto');

function prefix(ip) {
  if (ip.indexOf(':') >= 0) {
    var seg = ip.split(':');
    return seg[0] + ':' + (seg[1] || '0') + ':' + (seg[2] || '0') + '::/48';
  }
  var o = ip.split('.');
  return o[0] + '.' + o[1] + '.' + o[2] + '.0/24';
}

function token(r) {
  var key = Buffer.from(r.variables.dedan_key, 'hex');
  var minute = Math.floor(Date.now() / 60000);
  var mac = crypto.createHmac('sha256', key).update(minute + '|' + prefix(r.remoteAddress)).digest('hex');
  return r.variables.dedan_label + '.' + minute + '.' + mac;
}

export default { token };
