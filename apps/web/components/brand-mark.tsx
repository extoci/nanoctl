export function BrandMark({ wordmark = true }: { wordmark?: boolean }) {
  return (
    <span className="brand">
      <span className="brand-mark" aria-hidden="true">
        n
      </span>
      {wordmark ? <span className="brand-word">nanoctl</span> : null}
    </span>
  );
}
