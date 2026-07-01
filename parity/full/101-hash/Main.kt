// Smoke test for parity/101-hash — exercises the KotlinCrypto/hash
// public surface across two distinct digest families (SHA-2 and
// SHA-3) so the harness reports `pass` only if the project's lib
// classes actually loaded AND produced the correct digest bytes.
//
// The test vectors are NIST's canonical "abc" examples:
//   SHA-256("abc")     = ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad
//   SHA3-256("abc")    = 3a985da74fe225b2045c172d6bd390bd855f086e3e9d525b46bfe24511431532
//   MD5("abc")         = 900150983cd24fb0d6963f7d28e17f72

import org.kotlincrypto.hash.sha2.SHA256
import org.kotlincrypto.hash.sha3.SHA3_256
import org.kotlincrypto.hash.md.MD5

private fun bytesToHex(bytes: ByteArray): String {
    val hex = "0123456789abcdef"
    val sb = StringBuilder(bytes.size * 2)
    for (b in bytes) {
        val v = b.toInt() and 0xFF
        sb.append(hex[v ushr 4])
        sb.append(hex[v and 0x0F])
    }
    return sb.toString()
}

private fun check(label: String, hex: String, expected: String) {
    val ok = hex == expected
    println("$label=$hex ${if (ok) "OK" else "FAIL (expected $expected)"}")
}

fun main() {
    val input = "abc".encodeToByteArray()

    val sha256 = SHA256()
    sha256.update(input)
    check("sha256", bytesToHex(sha256.digest()),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad")

    val sha3 = SHA3_256()
    sha3.update(input)
    check("sha3_256", bytesToHex(sha3.digest()),
        "3a985da74fe225b2045c172d6bd390bd855f086e3e9d525b46bfe24511431532")

    val md5 = MD5()
    md5.update(input)
    check("md5", bytesToHex(md5.digest()),
        "900150983cd24fb0d6963f7d28e17f72")

    println("blockSize=${sha256.blockSize()} digestLength=${sha256.digestLength()}")
    println("algorithm=${sha256.algorithm()}")
}
