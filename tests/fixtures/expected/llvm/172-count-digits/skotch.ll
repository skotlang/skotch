; ModuleID = 'InputKt'
source_filename = "InputKt.kt"

@.fmt.int_println = private unnamed_addr constant [4 x i8] c"%d\0A\00", align 1

declare i32 @printf(ptr, ...)

define i32 @InputKt_countDigits(i32 %arg0) {
entry:
  %merge_7 = alloca i32
  %merge_9 = alloca i32
  %t0 = add i32 0, 0
  %t1 = icmp eq i32 %arg0, %t0
  br i1 %t1, label %bb1, label %bb2
bb1:
  %t2 = add i32 0, 1
  ret i32 %t2
bb2:
  br label %bb3
bb3:
  %t3 = add i32 0, 0
  store i32 %t3, ptr %merge_7
  store i32 %arg0, ptr %merge_9
  %t4 = load i32, ptr %merge_9
  %t5 = add i32 0, 0
  %t6 = icmp slt i32 %t4, %t5
  br i1 %t6, label %bb4, label %bb5
bb4:
  %t7 = load i32, ptr %merge_9
  %t8 = add i32 0, 0
  %t9 = sub i32 %t8, %t7
  store i32 %t9, ptr %merge_9
  br label %bb6
bb5:
  br label %bb6
bb6:
  br label %bb7
bb7:
  %t10 = load i32, ptr %merge_9
  %t11 = add i32 0, 0
  %t12 = icmp sgt i32 %t10, %t11
  br i1 %t12, label %bb8, label %bb9
bb8:
  %t13 = load i32, ptr %merge_9
  %t14 = add i32 0, 10
  %t15 = sdiv i32 %t13, %t14
  store i32 %t15, ptr %merge_9
  %t16 = load i32, ptr %merge_7
  %t17 = add i32 0, 1
  %t18 = add i32 %t16, %t17
  store i32 %t18, ptr %merge_7
  br label %bb7
bb9:
  %t19 = load i32, ptr %merge_7
  ret i32 %t19
}

define i32 @main() {
entry:
  %t0 = add i32 0, 0
  %t1 = call i32 @InputKt_countDigits(i32 %t0)
  call i32 (ptr, ...) @printf(ptr @.fmt.int_println, i32 %t1)
  %t3 = add i32 0, 7
  %t4 = call i32 @InputKt_countDigits(i32 %t3)
  call i32 (ptr, ...) @printf(ptr @.fmt.int_println, i32 %t4)
  %t6 = add i32 0, 42
  %t7 = call i32 @InputKt_countDigits(i32 %t6)
  call i32 (ptr, ...) @printf(ptr @.fmt.int_println, i32 %t7)
  %t9 = add i32 0, 12345
  %t10 = call i32 @InputKt_countDigits(i32 %t9)
  call i32 (ptr, ...) @printf(ptr @.fmt.int_println, i32 %t10)
  %t12 = add i32 0, -99
  %t13 = call i32 @InputKt_countDigits(i32 %t12)
  call i32 (ptr, ...) @printf(ptr @.fmt.int_println, i32 %t13)
  ret i32 0
}

