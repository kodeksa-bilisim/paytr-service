-- İndirim kodu silme. Kullanılmamış kod tamamen silinir; kullanılmış ya da ödeme penceresi açık
-- kod (coupon_redemptions ve abonelik metadata'sı ona bağlı) arşivlenir: listede ve ödemede görünmez.
ALTER TABLE coupons ADD COLUMN IF NOT EXISTS deleted_at TIMESTAMP;
