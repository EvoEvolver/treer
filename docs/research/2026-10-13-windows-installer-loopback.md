# Windows installer IPv6 loopback compatibility gate

Native workflow 38361567064 for ee45870 passed the pwsh staging and installer tests, but failed Windows PowerShell 5.1 installer compatibility. The first three allowed origins (HTTPS, localhost HTTP and IPv4 loopback HTTP) installed successfully; the IPv6 loopback case reached the cleartext-origin rejection. This suggests a difference in Uri host text representation between .NET Framework and modern .NET; the initial log does not print the normalized Host, so its exact text is not established.

The guard now checks IPv6 HostNameType and compares the parsed DnsSafeHost address with IPAddress.IPv6Loopback. It does not resolve DNS or broaden the exact localhost/127.0.0.1 IPv4 list. UserInfo rejection and checksums remain required. Tests retain all earlier guards and add expanded IPv6 loopback acceptance, non-loopback IPv6 and mapped IPv4 rejection. Initial script retrieval and redirect chains remain outside this origin guard.

Local source/docs/diff and Unix installer tests passed. This Mac has neither native Windows PowerShell nor pwsh; local checks are not Windows evidence. Both native shell jobs must run and pass against the fixed candidate before Canary deployment. Original failure logs remain under .treer/releases/ee45870e20615033111a3790c18cc2b713b3e47b/.

The follow-up native matrix also asserts that 127.0.0.2 HTTP is rejected (the IPv4 exception is exactly normalized 127.0.0.1), a nonzero scoped IPv6 loopback is rejected, uppercase LOCALHOST is accepted, and percent-encoded invalid IPv6 is rejected by URI validation. Case labels are numeric and never print URLs or credentials. These added assertions require fresh native results on both shell runtimes; the earlier e98d374 pass did not execute them.
