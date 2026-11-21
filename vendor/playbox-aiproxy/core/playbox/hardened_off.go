//go:build !playbox_hardened

package playbox

// Hardened is false in unhardened upstream builds.
const Hardened = false
