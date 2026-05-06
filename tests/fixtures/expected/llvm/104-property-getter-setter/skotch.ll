; ModuleID = 'InputKt'
source_filename = "InputKt.kt"

@.fmt.int_println = private unnamed_addr constant [4 x i8] c"%d\0A\00", align 1

declare i32 @puts(ptr)

define i32 @main() {
entry:
  %t0 = fadd double 0.0, 1e2
  %t1 = inttoptr i64 0 to ptr
  %t2 = inttoptr i64 0 to ptr
  call i32 @puts(ptr %t2)
  ret i32 0
}

