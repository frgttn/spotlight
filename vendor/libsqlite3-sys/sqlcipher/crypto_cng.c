/*
** SQLCipher crypto provider on Windows CNG (bcrypt.dll), so SQLCipher builds
** on Windows without OpenSSL. Added for Spotlight; not part of SQLCipher.
**
** Built with SQLCIPHER_CRYPTO_CUSTOM=sqlcipher_cng_setup. It is the same
** cipher as SQLCipher's other providers (AES-256-CBC, HMAC-SHA1/256/512,
** PBKDF2), so databases are interchangeable with any SQLCipher 4 build.
**
** Uses CNG's pseudo-handles (Windows 10 and later), so there is no algorithm
** provider to open, cache or close, and no per-connection state.
*/
#include <windows.h>
#include <bcrypt.h>

#include "sqlite3.h"

/* From SQLCipher's internal sqlcipher.h, which the amalgamation doesn't ship
** as a header; must match the copy in sqlite3.c. */
#define SQLCIPHER_ENCRYPT 1
#define SQLCIPHER_HMAC_SHA1 0
#define SQLCIPHER_HMAC_SHA256 1
#define SQLCIPHER_HMAC_SHA512 2

typedef struct sqlcipher_provider sqlcipher_provider;
struct sqlcipher_provider {
  int (*init)(void);
  void (*shutdown)(void);
  const char* (*get_provider_name)(void *ctx);
  int (*add_random)(void *ctx, const void *buffer, int length);
  int (*random)(void *ctx, void *buffer, int length);
  int (*hmac)(void *ctx, int algorithm,
              const unsigned char *hmac_key, int key_sz,
              const unsigned char *in, int in_sz,
              const unsigned char *in2, int in2_sz,
              unsigned char *out);
  int (*kdf)(void *ctx, int algorithm,
              const unsigned char *pass, int pass_sz,
              const unsigned char* salt, int salt_sz,
              int workfactor,
              int key_sz, unsigned char *key);
  int (*cipher)(void *ctx, int mode,
              const unsigned char *key, int key_sz,
              const unsigned char *iv,
              const unsigned char *in, int in_sz,
              unsigned char *out);
  const char* (*get_cipher)(void *ctx);
  int (*get_key_sz)(void *ctx);
  int (*get_iv_sz)(void *ctx);
  int (*get_block_sz)(void *ctx);
  int (*get_hmac_sz)(void *ctx, int algorithm);
  int (*ctx_init)(void **ctx);
  int (*ctx_free)(void **ctx);
  int (*fips_status)(void *ctx);
  const char* (*get_provider_version)(void *ctx);
  sqlcipher_provider *next;
};

#define AES_KEY_SZ 32
#define AES_BLOCK_SZ 16

#define OK(status) ((status) >= 0)

static int cng_get_hmac_sz(void *ctx, int algorithm);

/* The HMAC pseudo-handle for SQLCipher's algorithm number, or NULL. */
static BCRYPT_ALG_HANDLE cng_hmac_alg(int algorithm) {
  switch(algorithm) {
    case SQLCIPHER_HMAC_SHA1: return BCRYPT_HMAC_SHA1_ALG_HANDLE;
    case SQLCIPHER_HMAC_SHA256: return BCRYPT_HMAC_SHA256_ALG_HANDLE;
    case SQLCIPHER_HMAC_SHA512: return BCRYPT_HMAC_SHA512_ALG_HANDLE;
    default: return NULL;
  }
}

static int cng_add_random(void *ctx, const void *buffer, int length) {
  return SQLITE_OK;
}

static int cng_random(void *ctx, void *buffer, int length) {
  NTSTATUS status = BCryptGenRandom(NULL, (PUCHAR)buffer, (ULONG)length,
                                    BCRYPT_USE_SYSTEM_PREFERRED_RNG);
  return OK(status) ? SQLITE_OK : SQLITE_ERROR;
}

static const char* cng_get_provider_name(void *ctx) {
  return "cng";
}

static const char* cng_get_provider_version(void *ctx) {
  return "bcrypt";
}

static int cng_hmac(
  void *ctx, int algorithm,
  const unsigned char *hmac_key, int key_sz,
  const unsigned char *in, int in_sz,
  const unsigned char *in2, int in2_sz,
  unsigned char *out
) {
  BCRYPT_ALG_HANDLE alg = cng_hmac_alg(algorithm);
  BCRYPT_HASH_HANDLE hash = NULL;
  ULONG out_sz = (ULONG)cng_get_hmac_sz(ctx, algorithm);
  NTSTATUS status;
  if(in == NULL || alg == NULL) return SQLITE_ERROR;
  status = BCryptCreateHash(alg, &hash, NULL, 0, (PUCHAR)hmac_key, (ULONG)key_sz, 0);
  if(OK(status)) status = BCryptHashData(hash, (PUCHAR)in, (ULONG)in_sz, 0);
  if(OK(status) && in2 != NULL) status = BCryptHashData(hash, (PUCHAR)in2, (ULONG)in2_sz, 0);
  if(OK(status)) status = BCryptFinishHash(hash, out, out_sz, 0);
  if(hash != NULL) BCryptDestroyHash(hash);
  return OK(status) ? SQLITE_OK : SQLITE_ERROR;
}

/* PBKDF2 with the HMAC of `algorithm` (SQLCipher numbers its PBKDF2
** variants like its HMACs). */
static int cng_kdf(
  void *ctx, int algorithm,
  const unsigned char *pass, int pass_sz,
  const unsigned char* salt, int salt_sz,
  int workfactor,
  int key_sz, unsigned char *key
) {
  BCRYPT_ALG_HANDLE alg = cng_hmac_alg(algorithm);
  NTSTATUS status;
  if(alg == NULL) return SQLITE_ERROR;
  status = BCryptDeriveKeyPBKDF2(alg, (PUCHAR)pass, (ULONG)pass_sz, (PUCHAR)salt, (ULONG)salt_sz,
                                 (ULONGLONG)workfactor, key, (ULONG)key_sz, 0);
  return OK(status) ? SQLITE_OK : SQLITE_ERROR;
}

/* AES-256-CBC without padding: SQLCipher passes whole blocks. */
static int cng_cipher(
  void *ctx, int mode,
  const unsigned char *key, int key_sz,
  const unsigned char *iv,
  const unsigned char *in, int in_sz,
  unsigned char *out
) {
  BCRYPT_KEY_HANDLE handle = NULL;
  /* CNG updates the IV in place; SQLCipher's must stay as it is. */
  UCHAR iv_copy[AES_BLOCK_SZ];
  ULONG done = 0;
  NTSTATUS status;
  if(key_sz != AES_KEY_SZ || in_sz % AES_BLOCK_SZ != 0) return SQLITE_ERROR;
  memcpy(iv_copy, iv, AES_BLOCK_SZ);
  status = BCryptGenerateSymmetricKey(BCRYPT_AES_CBC_ALG_HANDLE, &handle, NULL, 0,
                                      (PUCHAR)key, (ULONG)key_sz, 0);
  if(OK(status)) {
    if(mode == SQLCIPHER_ENCRYPT) {
      status = BCryptEncrypt(handle, (PUCHAR)in, (ULONG)in_sz, NULL, iv_copy, AES_BLOCK_SZ,
                             out, (ULONG)in_sz, &done, 0);
    } else {
      status = BCryptDecrypt(handle, (PUCHAR)in, (ULONG)in_sz, NULL, iv_copy, AES_BLOCK_SZ,
                             out, (ULONG)in_sz, &done, 0);
    }
  }
  if(handle != NULL) BCryptDestroyKey(handle);
  SecureZeroMemory(iv_copy, sizeof(iv_copy));
  return OK(status) && done == (ULONG)in_sz ? SQLITE_OK : SQLITE_ERROR;
}

static const char* cng_get_cipher(void *ctx) {
  return "aes-256-cbc";
}

static int cng_get_key_sz(void *ctx) {
  return AES_KEY_SZ;
}

static int cng_get_iv_sz(void *ctx) {
  return AES_BLOCK_SZ;
}

static int cng_get_block_sz(void *ctx) {
  return AES_BLOCK_SZ;
}

static int cng_get_hmac_sz(void *ctx, int algorithm) {
  switch(algorithm) {
    case SQLCIPHER_HMAC_SHA1: return 20;
    case SQLCIPHER_HMAC_SHA256: return 32;
    case SQLCIPHER_HMAC_SHA512: return 64;
    default: return 0;
  }
}

static int cng_ctx_init(void **ctx) {
  return SQLITE_OK;
}

static int cng_ctx_free(void **ctx) {
  return SQLITE_OK;
}

static int cng_fips_status(void *ctx) {
  return 0;
}

int sqlcipher_cng_setup(sqlcipher_provider *p) {
  p->init = NULL;
  p->shutdown = NULL;
  p->random = cng_random;
  p->get_provider_name = cng_get_provider_name;
  p->hmac = cng_hmac;
  p->kdf = cng_kdf;
  p->cipher = cng_cipher;
  p->get_cipher = cng_get_cipher;
  p->get_key_sz = cng_get_key_sz;
  p->get_iv_sz = cng_get_iv_sz;
  p->get_block_sz = cng_get_block_sz;
  p->get_hmac_sz = cng_get_hmac_sz;
  p->ctx_init = cng_ctx_init;
  p->ctx_free = cng_ctx_free;
  p->add_random = cng_add_random;
  p->fips_status = cng_fips_status;
  p->get_provider_version = cng_get_provider_version;
  return SQLITE_OK;
}
