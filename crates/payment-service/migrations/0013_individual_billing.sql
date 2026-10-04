-- Bireysel faturada düzenlenebilir ad soyad (boşsa hesap adı kullanılır). Bireyselde
-- tax_number = TCKN (isteğe bağlı), adres/il/ilçe isteğe bağlı (hepsi birlikte).
ALTER TABLE billing_profiles ADD COLUMN IF NOT EXISTS full_name VARCHAR(120);
