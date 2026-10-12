"""Shared conservative byte scanner; never report matched bytes."""
import re

SECRET_PATTERNS = {
    "treer_workload": rb"\bwlc_[A-Za-z0-9_-]{8,}",
    "treer_machine": rb"\bsrv_[A-Za-z0-9_-]+\.[A-Za-z0-9_-]{16,}",
    "treer_enrollment_key": rb"\benr_v1_[A-Za-z0-9_]+\.[a-fA-F0-9]{64}\b",
    "treer_enrollment": rb"(?i)(?:enrollment|machine)[_-]?(?:token|secret|credential)[\s\"']*[:=][\s\"']*[A-Za-z0-9_.-]{8,}",
    "api_token": rb"\b(?:sk-[A-Za-z0-9_-]{8,}|ghp_[A-Za-z0-9]{8,}|github_pat_[A-Za-z0-9_]{8,})",
    "bearer": rb"(?i)\bBearer\s+[A-Za-z0-9_.~+/-]{8,}=*",
    "jwt": rb"\beyJ[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+",
    "private_key": rb"-----BEGIN (?:[A-Z0-9]+ )?PRIVATE KEY-----",
    "aws_key_id": rb"\b(?:AKIA|ASIA)[A-Z0-9]{16}\b",
    "secret_assignment": rb"(?i)\b(?:password|secret|api[_-]?key)[\s\"']*[:=][\s\"']*[^\s\"',;}]{3,}",
}


def scan_secrets(data):
    return {category: len(re.findall(pattern, data)) for category, pattern in SECRET_PATTERNS.items()
            if re.search(pattern, data)}


def redact(data):
    counts = {}
    # Remove complete private-key blocks before individual token patterns.
    pattern = rb"-----BEGIN (?:[A-Z0-9]+ )?PRIVATE KEY-----[\s\S]*?-----END (?:[A-Z0-9]+ )?PRIVATE KEY-----"
    data, count = re.subn(pattern, b"[REDACTED:private_key]", data)
    if count:
        counts['private_key'] = count
    for category, pattern in SECRET_PATTERNS.items():
        data, count = re.subn(pattern, ('[REDACTED:' + category + ']').encode(), data)
        if count:
            counts[category] = counts.get(category, 0) + count
    return data, counts
