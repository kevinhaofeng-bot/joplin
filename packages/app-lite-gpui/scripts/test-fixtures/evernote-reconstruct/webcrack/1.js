let o = require("./2.js");
let i = (0, o.createLogger)("boron:demoController");
class s {
  start() {
    i.info("started");
    return o.value;
  }
}
exports.default = s;
