(() => {
    var __webpack_modules__ = {
        1: function(t, a, n) {
            "use strict";
            let o = n(2);
            let i = (0, o.createLogger)("boron:demoController");
            class s {
                start() {
                    i.info("started");
                    return o.value;
                }
            }
            a.default = s;
        },
        2: function(t, a) {
            "use strict";
            a.value = 42;
        },
        7e3: function(t, a) {
            "use strict";
            a.read = function(value) {
                "use strict";
                void 0 === value && (value = this.fallback);
                return value;
            };
        }
    }, __webpack_module_cache__ = {};
    function __webpack_require__(t) {
        return __webpack_modules__[t]({}, {}, __webpack_require__);
    }
    var __webpack_exports__ = {};
    (() => {
        "use strict";
        __webpack_require__(2);
        __webpack_require__(7000);
        let t = __webpack_require__(1);
        let a = new t.default;
        __webpack_exports__.result = a.start();
    })(), module.exports = __webpack_exports__;
})();
