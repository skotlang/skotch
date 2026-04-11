; ModuleID = 'InputKt'
source_filename = "InputKt.kt"

@.fmt.int_println = private unnamed_addr constant [4 x i8] c"%d\0A\00", align 1

declare i32 @printf(ptr, ...)

define i32 @InputKt_double(i32 %arg0) {
entry:
  %t0 = add i32 0, 2
  %t1 = mul i32 %arg0, %t0
  ret i32 %t1
}

define i32 @InputKt_addOne(i32 %arg0) {
entry:
  %t0 = add i32 0, 1
  %t1 = add i32 %arg0, %t0
  ret i32 %t1
}

define i32 @main() {
entry:
  %t0 = add i32 0, 5
  %t1 = call i32 @InputKt_double(i32 %t0)
  %t2 = call i32 @InputKt_addOne(i32 %t1)
  call i32 (ptr, ...) @printf(ptr @.fmt.int_println, i32 %t2)
  %t4 = add i32 0, 3
  %t5 = call i32 @InputKt_addOne(i32 %t4)
  %t6 = call i32 @InputKt_double(i32 %t5)
  call i32 (ptr, ...) @printf(ptr @.fmt.int_println, i32 %t6)
  ret i32 0
}

